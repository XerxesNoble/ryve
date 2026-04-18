// SPDX-License-Identifier: AGPL-3.0-or-later

//! IRC outbox relay — drain → filter → render → send → persist → mark.
//!
//! This module is the single code path that writes to IRC. Every row the
//! relay accepts comes from `event_outbox`; every line that lands on the
//! wire is produced by [`crate::irc_renderer::event_to_irc`] and sent on
//! an [`crate::irc_client::IrcClient`] owned privately by the relay.
//!
//! ## Pipeline
//!
//! For each pending outbox row the relay:
//! 1. Checks [`crate::signal_discipline::is_allowed`]; non-allow-list
//!    events are marked `sent` (skipped) with no IRC emission.
//! 2. Parses the row's event_type + payload JSON into the renderer's
//!    typed [`crate::irc_renderer::OutboxEvent`].
//! 3. Renders the event to an [`crate::irc_renderer::IrcLine`].
//! 4. Sends `send_privmsg(channel, text)` on the IRC client.
//! 5. Persists the emission via [`data::sparks::irc_repo::insert_message`].
//! 6. Marks the outbox state row `sent` with `sent_at` stamped.
//!
//! ## Failure semantics
//!
//! Any failure in steps 3–5 increments `attempts` and records `last_error`.
//! The state row stays `pending` (so the next `fetch_pending` cycle picks
//! it up again) until `attempts >= max_attempts`, at which point the row
//! transitions to the terminal `failed` state and the relay emits a
//! `flare` ember identifying the stuck event so an operator can intervene.
//!
//! ## Durability
//!
//! Dropping the IRC server does not lose events: sends fail, rows stay in
//! `pending` with incremented `attempts`, and subsequent cycles keep
//! trying up to `max_attempts`. The [`crate::irc_client::IrcClient`]
//! itself also queues in-flight sends during disconnects and replays on
//! reconnect.
//!
//! ## Idempotency
//!
//! The state table is keyed on `event_id`. Replaying the loop over a row
//! already marked `sent` is a no-op — the SELECT in [`fetch_pending`]
//! excludes it.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use data::sparks::types::{EmberType, IrcCommand as DbIrcCommand, NewEmber, NewIrcMessage};
use data::sparks::{ember_repo, irc_repo};
use serde::Deserialize;
use sqlx::SqlitePool;
use thiserror::Error;

use crate::irc_client::{IrcClient, IrcError};
use crate::irc_renderer::{
    self, EpicRef, EventPayload, IrcCommand, IrcLine, OutboxEvent, PrReviewState, ReviewOutcome,
};
use crate::signal_discipline;

/// Tunables for [`run`] and [`RelayHandle::drain_once`].
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// Delay between drain passes when the outbox had no work.
    pub poll_interval: Duration,
    /// How many retries a single event may accumulate before the relay
    /// gives up and emits a flare ember. `1` means "try once, never
    /// retry". Defaults to `5`.
    pub max_attempts: u32,
    /// Upper bound on rows fetched in a single drain pass.
    pub batch_size: i64,
    /// Workshop id stamped on flare embers emitted when a row exhausts
    /// its retry budget. Matches the embers convention of scoping signals
    /// to a workshop.
    pub workshop_id: String,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            max_attempts: 5,
            batch_size: 100,
            workshop_id: "default".to_string(),
        }
    }
}

/// Outcome of a single [`RelayHandle::drain_once`] pass. Exposed so tests
/// can assert the per-pass bookkeeping without peeking into the DB.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainOutcome {
    /// Rows fetched from `event_outbox` for consideration.
    pub fetched: usize,
    /// Rows that passed the signal-discipline filter and were delivered
    /// end-to-end (rendered, sent, persisted, state → sent).
    pub sent: usize,
    /// Rows dropped by the signal-discipline filter (state → sent,
    /// never placed on the wire).
    pub skipped_filtered: usize,
    /// Rows whose delivery failed this pass (state → failed, attempts
    /// incremented). Includes permanent failures that emitted a flare
    /// ember.
    pub failed: usize,
    /// Rows that tripped the `max_attempts` cliff on this pass — a
    /// subset of `failed`. One flare ember is emitted per row counted
    /// here.
    pub flared: usize,
}

/// Errors that can bubble out of [`run`]. Delivery failures are handled
/// per-row inside the loop and never reach this type.
#[derive(Debug, Error)]
pub enum RelayError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("sparks error: {0}")]
    Sparks(#[from] data::sparks::error::SparksError),
}

/// Long-lived task. Loops until the process exits, draining the outbox
/// at [`RelayConfig::poll_interval`] cadence. Any database error aborts
/// the task; per-row send/render failures are recorded and retried.
pub async fn run(
    pool: SqlitePool,
    client: Arc<IrcClient>,
    config: RelayConfig,
) -> Result<(), RelayError> {
    let handle = RelayHandle::new(pool, client, config);
    loop {
        handle.drain_once().await?;
        tokio::time::sleep(handle.config.poll_interval).await;
    }
}

/// Test-friendly handle that exposes [`Self::drain_once`] for single-pass
/// assertions. [`run`] is a thin loop over this.
pub struct RelayHandle {
    pool: SqlitePool,
    client: Arc<IrcClient>,
    config: RelayConfig,
}

impl RelayHandle {
    pub fn new(pool: SqlitePool, client: Arc<IrcClient>, config: RelayConfig) -> Self {
        Self {
            pool,
            client,
            config,
        }
    }

    /// Run exactly one drain pass against the current outbox.
    pub async fn drain_once(&self) -> Result<DrainOutcome, RelayError> {
        let rows = self.fetch_pending().await?;
        let mut outcome = DrainOutcome {
            fetched: rows.len(),
            ..DrainOutcome::default()
        };

        for row in rows {
            match self.process_one(&row).await? {
                StepOutcome::Sent => outcome.sent += 1,
                StepOutcome::SkippedFiltered => outcome.skipped_filtered += 1,
                StepOutcome::Failed { flared } => {
                    outcome.failed += 1;
                    if flared {
                        outcome.flared += 1;
                    }
                }
            }
        }
        Ok(outcome)
    }

    async fn process_one(&self, row: &OutboxRow) -> Result<StepOutcome, RelayError> {
        if !signal_discipline::is_allowed(&row.event_type) {
            self.mark_sent(&row.event_id, row.attempts).await?;
            return Ok(StepOutcome::SkippedFiltered);
        }

        let event = match parse_event(row) {
            Ok(ev) => ev,
            Err(err) => {
                return self
                    .record_failure(row, format!("parse error: {err}"))
                    .await;
            }
        };

        let line = match irc_renderer::event_to_irc(&event) {
            Some(line) => line,
            None => {
                // Renderer opted not to emit — treat the same as signal-
                // discipline skip.
                self.mark_sent(&row.event_id, row.attempts).await?;
                return Ok(StepOutcome::SkippedFiltered);
            }
        };

        if let Err(err) = self.send(&line).await {
            return self
                .record_failure(row, format!("irc send failed: {err}"))
                .await;
        }

        if let Err(err) = self.persist(&event, &line).await {
            return self
                .record_failure(row, format!("irc_repo insert failed: {err}"))
                .await;
        }

        self.mark_sent(&row.event_id, row.attempts).await?;
        Ok(StepOutcome::Sent)
    }

    async fn send(&self, line: &IrcLine) -> Result<(), IrcError> {
        match line.command {
            IrcCommand::Privmsg => self.client.send_privmsg(&line.channel, &line.text).await,
        }
    }

    async fn persist(
        &self,
        event: &OutboxEvent,
        line: &IrcLine,
    ) -> Result<(), data::sparks::error::SparksError> {
        irc_repo::insert_message(
            &self.pool,
            NewIrcMessage {
                epic_id: event.epic.id.clone(),
                channel: line.channel.clone(),
                // The IRC server assigns its own ids; our durable log
                // keys on the originating outbox event_id so replay
                // joins back to the state row.
                irc_message_id: event.event_id.clone(),
                sender_actor_id: None,
                command: match line.command {
                    IrcCommand::Privmsg => DbIrcCommand::Privmsg,
                },
                raw_text: line.text.clone(),
                structured_event_id: Some(event.event_id.clone()),
            },
        )
        .await?;
        Ok(())
    }

    async fn fetch_pending(&self) -> Result<Vec<OutboxRow>, sqlx::Error> {
        let rows = sqlx::query_as::<_, OutboxRow>(
            "SELECT o.event_id, o.event_type, o.payload, \
                    COALESCE(s.attempts, 0) AS attempts \
             FROM event_outbox o \
             LEFT JOIN irc_outbox_state s ON s.event_id = o.event_id \
             WHERE COALESCE(s.status, 'pending') != 'sent' \
               AND COALESCE(s.status, 'pending') != 'failed' \
             ORDER BY o.timestamp ASC, o.event_id ASC \
             LIMIT ?",
        )
        .bind(self.config.batch_size)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn mark_sent(&self, event_id: &str, attempts: i64) -> Result<(), sqlx::Error> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO irc_outbox_state \
                (event_id, status, attempts, last_error, sent_at, updated_at) \
             VALUES (?, 'sent', ?, NULL, ?, ?) \
             ON CONFLICT(event_id) DO UPDATE SET \
                status='sent', attempts=excluded.attempts, \
                last_error=NULL, sent_at=excluded.sent_at, \
                updated_at=excluded.updated_at",
        )
        .bind(event_id)
        .bind(attempts)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn record_failure(
        &self,
        row: &OutboxRow,
        reason: String,
    ) -> Result<StepOutcome, RelayError> {
        let attempts = row.attempts + 1;
        let exhausted = attempts as u32 >= self.config.max_attempts;
        // Keep retryable rows in 'pending' so `fetch_pending` picks them up
        // again on the next cycle. Only flip to the terminal 'failed' state
        // once the retry budget is exhausted — the migration 020 contract
        // and module docs both treat 'failed' as "max_attempts reached".
        let status = if exhausted { "failed" } else { "pending" };
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO irc_outbox_state \
                (event_id, status, attempts, last_error, sent_at, updated_at) \
             VALUES (?, ?, ?, ?, NULL, ?) \
             ON CONFLICT(event_id) DO UPDATE SET \
                status=excluded.status, attempts=excluded.attempts, \
                last_error=excluded.last_error, \
                updated_at=excluded.updated_at",
        )
        .bind(&row.event_id)
        .bind(status)
        .bind(attempts)
        .bind(&reason)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        let mut flared = false;
        if exhausted {
            self.emit_flare(&row.event_id, &reason, attempts).await?;
            flared = true;
        }
        Ok(StepOutcome::Failed { flared })
    }

    async fn emit_flare(
        &self,
        event_id: &str,
        reason: &str,
        attempts: i64,
    ) -> Result<(), data::sparks::error::SparksError> {
        ember_repo::create(
            &self.pool,
            NewEmber {
                ember_type: EmberType::Flare,
                content: format!(
                    "IRC relay: event {event_id} stuck after {attempts} attempts: {reason}"
                ),
                source_agent: Some("outbox_relay".to_string()),
                workshop_id: self.config.workshop_id.clone(),
                ttl_seconds: None,
            },
        )
        .await?;
        Ok(())
    }
}

enum StepOutcome {
    Sent,
    SkippedFiltered,
    Failed { flared: bool },
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct OutboxRow {
    event_id: String,
    event_type: String,
    payload: String,
    attempts: i64,
}

/// Parse errors — any is reported as a failure and the row retries. Not
/// exposed in the public API because the relay never surfaces them.
#[derive(Debug, Error)]
enum ParseError {
    #[error("unknown event_type")]
    UnknownEventType,
    #[error("malformed payload: {0}")]
    Payload(#[from] serde_json::Error),
}

/// JSON payload the relay expects on every allow-listed outbox row.
///
/// Producers writing to `event_outbox` must include `epic_id` and
/// `epic_name` alongside the event-specific fields. The renderer derives
/// the target channel from the `EpicRef`, so a row without them cannot
/// route anywhere.
#[derive(Debug, Deserialize)]
struct PayloadEnvelope {
    epic_id: String,
    epic_name: String,
    #[serde(flatten)]
    fields: serde_json::Value,
}

fn parse_event(row: &OutboxRow) -> Result<OutboxEvent, ParseError> {
    let env: PayloadEnvelope = serde_json::from_str(&row.payload)?;
    let epic = EpicRef {
        id: env.epic_id,
        name: env.epic_name,
    };
    let payload = parse_payload(&row.event_type, env.fields)?;
    Ok(OutboxEvent {
        event_id: row.event_id.clone(),
        epic,
        payload,
    })
}

fn parse_payload(event_type: &str, value: serde_json::Value) -> Result<EventPayload, ParseError> {
    use serde_json::from_value;
    Ok(match event_type {
        "assignment.created" => {
            #[derive(Deserialize)]
            struct F {
                assignment_id: String,
                actor: String,
            }
            let f: F = from_value(value)?;
            EventPayload::AssignmentCreated {
                assignment_id: f.assignment_id,
                actor: f.actor,
            }
        }
        "assignment.transitioned" => {
            #[derive(Deserialize)]
            struct F {
                assignment_id: String,
                from: String,
                to: String,
                actor: String,
            }
            let f: F = from_value(value)?;
            EventPayload::AssignmentTransitioned {
                assignment_id: f.assignment_id,
                from: f.from,
                to: f.to,
                actor: f.actor,
            }
        }
        "assignment.stuck" => {
            #[derive(Deserialize)]
            struct F {
                assignment_id: String,
                reason: String,
            }
            let f: F = from_value(value)?;
            EventPayload::AssignmentStuck {
                assignment_id: f.assignment_id,
                reason: f.reason,
            }
        }
        "review.assigned" => {
            #[derive(Deserialize)]
            struct F {
                assignment_id: String,
                reviewer: String,
                kind: String,
            }
            let f: F = from_value(value)?;
            EventPayload::ReviewAssigned {
                assignment_id: f.assignment_id,
                reviewer: f.reviewer,
                kind: f.kind,
            }
        }
        "review.completed" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "snake_case", tag = "outcome")]
            enum Outcome {
                Approved,
                Rejected { code: String, location: String },
            }
            #[derive(Deserialize)]
            struct F {
                assignment_id: String,
                reviewer: String,
                #[serde(flatten)]
                outcome: Outcome,
            }
            let f: F = from_value(value)?;
            let outcome = match f.outcome {
                Outcome::Approved => ReviewOutcome::Approved,
                Outcome::Rejected { code, location } => ReviewOutcome::Rejected { code, location },
            };
            EventPayload::ReviewCompleted {
                assignment_id: f.assignment_id,
                reviewer: f.reviewer,
                outcome,
            }
        }
        "merge.started" => {
            #[derive(Deserialize)]
            struct F {
                epic_branch: String,
                sub_prs: Vec<u64>,
            }
            let f: F = from_value(value)?;
            EventPayload::MergeStarted {
                epic_branch: f.epic_branch,
                sub_prs: f.sub_prs,
            }
        }
        "merge.completed" => {
            #[derive(Deserialize)]
            struct F {
                epic_branch: String,
                merged_pr: u64,
            }
            let f: F = from_value(value)?;
            EventPayload::MergeCompleted {
                epic_branch: f.epic_branch,
                merged_pr: f.merged_pr,
            }
        }
        "epic.blocker_raised" => {
            #[derive(Deserialize)]
            struct F {
                assignment_id: String,
                reason: String,
            }
            let f: F = from_value(value)?;
            EventPayload::EpicBlockerRaised {
                assignment_id: f.assignment_id,
                reason: f.reason,
            }
        }
        "github.pr.opened" => {
            #[derive(Deserialize)]
            struct F {
                pr_number: u64,
                author: String,
                title: String,
            }
            let f: F = from_value(value)?;
            EventPayload::GithubPrOpened {
                pr_number: f.pr_number,
                author: f.author,
                title: f.title,
            }
        }
        "github.pr.closed" => {
            #[derive(Deserialize)]
            struct F {
                pr_number: u64,
                actor: String,
            }
            let f: F = from_value(value)?;
            EventPayload::GithubPrClosed {
                pr_number: f.pr_number,
                actor: f.actor,
            }
        }
        "github.pr.merged" => {
            #[derive(Deserialize)]
            struct F {
                pr_number: u64,
                actor: String,
            }
            let f: F = from_value(value)?;
            EventPayload::GithubPrMerged {
                pr_number: f.pr_number,
                actor: f.actor,
            }
        }
        "github.pr.review_requested" => {
            #[derive(Deserialize)]
            struct F {
                pr_number: u64,
                reviewer: String,
            }
            let f: F = from_value(value)?;
            EventPayload::GithubPrReviewRequested {
                pr_number: f.pr_number,
                reviewer: f.reviewer,
            }
        }
        "github.pr.review_submitted" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "snake_case")]
            enum State {
                Approved,
                ChangesRequested,
                Commented,
            }
            #[derive(Deserialize)]
            struct F {
                pr_number: u64,
                reviewer: String,
                state: State,
            }
            let f: F = from_value(value)?;
            EventPayload::GithubPrReviewSubmitted {
                pr_number: f.pr_number,
                reviewer: f.reviewer,
                state: match f.state {
                    State::Approved => PrReviewState::Approved,
                    State::ChangesRequested => PrReviewState::ChangesRequested,
                    State::Commented => PrReviewState::Commented,
                },
            }
        }
        "github.pr.comment_added" => {
            #[derive(Deserialize)]
            struct F {
                pr_number: u64,
                author: String,
                path: Option<String>,
                excerpt: String,
            }
            let f: F = from_value(value)?;
            EventPayload::GithubPrCommentAdded {
                pr_number: f.pr_number,
                author: f.author,
                path: f.path,
                excerpt: f.excerpt,
            }
        }
        _ => return Err(ParseError::UnknownEventType),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_event_extracts_epic_and_payload() {
        let row = OutboxRow {
            event_id: "evt-1".into(),
            event_type: "assignment.created".into(),
            payload: serde_json::json!({
                "epic_id": "42",
                "epic_name": "Checkout Refactor",
                "assignment_id": "asgn-1",
                "actor": "alice",
            })
            .to_string(),
            attempts: 0,
        };
        let event = parse_event(&row).unwrap();
        assert_eq!(event.epic.id, "42");
        assert_eq!(event.epic.name, "Checkout Refactor");
        matches!(event.payload, EventPayload::AssignmentCreated { .. });
    }

    #[test]
    fn parse_event_rejects_unknown_event_type() {
        let row = OutboxRow {
            event_id: "evt-1".into(),
            event_type: "something.unknown".into(),
            payload: r#"{"epic_id":"1","epic_name":"n"}"#.into(),
            attempts: 0,
        };
        assert!(matches!(
            parse_event(&row),
            Err(ParseError::UnknownEventType)
        ));
    }

    #[test]
    fn parse_event_rejects_missing_epic_fields() {
        let row = OutboxRow {
            event_id: "evt-1".into(),
            event_type: "assignment.created".into(),
            payload: r#"{"assignment_id":"asgn","actor":"a"}"#.into(),
            attempts: 0,
        };
        assert!(matches!(parse_event(&row), Err(ParseError::Payload(_))));
    }
}
