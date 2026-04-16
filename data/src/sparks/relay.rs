// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Outbox relay — drains `event_outbox` to registered subscribers.
//!
//! Every Assignment state transition appends a row to `event_outbox` inside
//! the same transaction as the state write. This module owns the *other* half
//! of that pattern: a background task that reads undelivered rows, hands
//! each one to every registered [`Subscriber`] (IRC bridge, GitHub mirror,
//! state projector, …), and stamps `delivered_at` once every subscriber
//! accepts the event.
//!
//! ## Durability & retry semantics
//!
//! - An event is marked `delivered_at` **only after every subscriber accepts
//!   it**. If any subscriber returns an error the row stays undelivered and
//!   will be re-attempted on the next relay pass.
//! - Failed deliveries are therefore never lost: the row remains in the
//!   table with `delivered_at IS NULL` until a future pass succeeds.
//! - Retry cadence per event is exponential with a configurable base and
//!   cap. Retry state is kept in memory for the lifetime of the relay; on
//!   restart the backoff resets, which is safe because subscribers must be
//!   idempotent (they may see the same `event_id` twice across restarts).
//! - Subscribers must be idempotent. The relay gives at-least-once delivery
//!   with `event_id` as the dedup key.
//!
//! ## Running
//!
//! [`Relay::run`] loops forever, draining a batch, sleeping
//! [`RelayConfig::poll_interval`], and repeating. For tests, [`Relay::drain_once`]
//! runs a single pass and returns the per-subscriber outcome so assertions
//! can observe retry/backoff behavior without sleeping wall-clock seconds.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::error::SparksError;

/// A single row drained from `event_outbox`, handed to subscribers verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq, Eq)]
pub struct OutboxEvent {
    pub event_id: String,
    pub schema_version: i64,
    pub timestamp: String,
    pub assignment_id: String,
    pub actor_id: String,
    pub event_type: String,
    pub payload: String,
}

/// Error returned by a subscriber when it cannot accept an event.
///
/// The relay treats every `DeliveryError` as a transient failure and retries
/// the event on a later pass with exponential backoff. Subscribers that want
/// to permanently reject an event should treat that as a bug in the event
/// schema and surface it via their own logs; the outbox has no concept of a
/// poison queue.
#[derive(Debug, thiserror::Error)]
#[error("delivery failed: {0}")]
pub struct DeliveryError(pub String);

impl DeliveryError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// Sink for outbox events. Implementations must be idempotent by `event_id`
/// because the relay provides at-least-once delivery.
pub trait Subscriber: Send + Sync {
    /// Stable identifier used in retry-state bookkeeping and logs.
    fn name(&self) -> &'static str;

    /// Try to deliver `event`. Any error is treated as transient and will be
    /// retried on the next pass.
    fn deliver<'a>(&'a self, event: &'a OutboxEvent) -> BoxFuture<'a, Result<(), DeliveryError>>;
}

/// Configuration knobs for [`Relay`].
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// How long to sleep between drain passes when the outbox was empty or
    /// every row was successfully delivered.
    pub poll_interval: Duration,
    /// Initial delay after the first delivery failure for an event.
    pub initial_backoff: Duration,
    /// Upper bound on backoff — delivery will never wait longer than this
    /// between attempts for a single event.
    pub max_backoff: Duration,
    /// Multiplier applied to the current backoff after each failure.
    pub backoff_multiplier: f64,
    /// Maximum number of rows drained in a single pass. Bounds memory use
    /// when the outbox has accumulated a large backlog.
    pub batch_size: i64,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(60),
            backoff_multiplier: 2.0,
            batch_size: 100,
        }
    }
}

/// Per-event retry bookkeeping kept in memory for the relay's lifetime.
#[derive(Debug, Clone, Copy)]
struct RetryState {
    /// Number of delivery attempts that have failed so far.
    attempts: u32,
    /// Earliest `Instant` at which the next attempt is allowed.
    next_attempt_at: Instant,
}

/// Outcome of a single [`Relay::drain_once`] pass. Exposed primarily so tests
/// can assert that delivery, retry, and backoff transitions happened exactly
/// as expected without inspecting internal state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainOutcome {
    /// Rows fetched from `event_outbox`.
    pub fetched: usize,
    /// Rows skipped because their backoff window had not yet elapsed.
    pub skipped_backoff: usize,
    /// Rows successfully delivered to every subscriber and stamped
    /// `delivered_at`.
    pub delivered: usize,
    /// Rows whose delivery failed for at least one subscriber. These remain
    /// in the table with `delivered_at IS NULL` and are scheduled for retry.
    pub failed: usize,
}

/// Drains `event_outbox` to its registered subscribers.
pub struct Relay {
    pool: SqlitePool,
    subscribers: Vec<Arc<dyn Subscriber>>,
    config: RelayConfig,
    /// Per-event retry state. Keyed by `event_id`. Entries are removed once
    /// an event is successfully delivered.
    retry: Arc<Mutex<HashMap<String, RetryState>>>,
}

impl Relay {
    pub fn new(
        pool: SqlitePool,
        subscribers: Vec<Arc<dyn Subscriber>>,
        config: RelayConfig,
    ) -> Self {
        Self {
            pool,
            subscribers,
            config,
            retry: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Run the relay forever, draining undelivered events and sleeping
    /// between passes. Intended to be spawned on a background task
    /// (`tokio::spawn(relay.run())`).
    pub async fn run(self) -> Result<(), SparksError> {
        loop {
            self.drain_once().await?;
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    /// Run a single drain pass. Fetches up to `batch_size` undelivered
    /// events, attempts delivery to every subscriber (honoring each event's
    /// backoff window), and stamps `delivered_at` for the ones that
    /// succeeded. Returns a [`DrainOutcome`] summarising what happened.
    pub async fn drain_once(&self) -> Result<DrainOutcome, SparksError> {
        let events = self.fetch_undelivered().await?;
        let mut outcome = DrainOutcome {
            fetched: events.len(),
            ..DrainOutcome::default()
        };

        let now = Instant::now();

        for event in events {
            if !self.ready_for_attempt(&event.event_id, now).await {
                outcome.skipped_backoff += 1;
                continue;
            }

            match self.deliver_to_all(&event).await {
                Ok(()) => {
                    self.mark_delivered(&event.event_id).await?;
                    self.retry.lock().await.remove(&event.event_id);
                    outcome.delivered += 1;
                }
                Err(_) => {
                    self.record_failure(&event.event_id).await;
                    outcome.failed += 1;
                }
            }
        }

        Ok(outcome)
    }

    async fn fetch_undelivered(&self) -> Result<Vec<OutboxEvent>, SparksError> {
        let rows = sqlx::query_as::<_, OutboxEvent>(
            "SELECT event_id, schema_version, timestamp, assignment_id, actor_id, event_type, payload
             FROM event_outbox
             WHERE delivered_at IS NULL
             ORDER BY timestamp ASC, event_id ASC
             LIMIT ?",
        )
        .bind(self.config.batch_size)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn ready_for_attempt(&self, event_id: &str, now: Instant) -> bool {
        let retry = self.retry.lock().await;
        match retry.get(event_id) {
            None => true,
            Some(state) => now >= state.next_attempt_at,
        }
    }

    async fn deliver_to_all(&self, event: &OutboxEvent) -> Result<(), DeliveryError> {
        for sub in &self.subscribers {
            sub.deliver(event).await.map_err(|e| {
                DeliveryError::new(format!("subscriber {} failed: {}", sub.name(), e.0))
            })?;
        }
        Ok(())
    }

    async fn mark_delivered(&self, event_id: &str) -> Result<(), SparksError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE event_outbox SET delivered_at = ? WHERE event_id = ?")
            .bind(&now)
            .bind(event_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn record_failure(&self, event_id: &str) {
        let mut retry = self.retry.lock().await;
        let state = retry.entry(event_id.to_string()).or_insert(RetryState {
            attempts: 0,
            next_attempt_at: Instant::now(),
        });
        state.attempts = state.attempts.saturating_add(1);
        state.next_attempt_at = Instant::now() + self.backoff_for(state.attempts);
    }

    fn backoff_for(&self, attempts: u32) -> Duration {
        compute_backoff(&self.config, attempts)
    }
}

/// Exponential backoff for the Nth failed attempt, clamped to `max_backoff`.
///
/// `attempts` is 1-indexed: `1` is the delay after the first failure,
/// `2` after the second, and so on.
fn compute_backoff(config: &RelayConfig, attempts: u32) -> Duration {
    let base = config.initial_backoff.as_secs_f64();
    let exp = attempts.saturating_sub(1) as i32;
    let scaled = base * config.backoff_multiplier.powi(exp);
    let capped = scaled.min(config.max_backoff.as_secs_f64());
    Duration::from_secs_f64(capped.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially_then_caps() {
        let config = RelayConfig {
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            ..RelayConfig::default()
        };

        assert_eq!(compute_backoff(&config, 1), Duration::from_millis(100));
        assert_eq!(compute_backoff(&config, 2), Duration::from_millis(200));
        assert_eq!(compute_backoff(&config, 3), Duration::from_millis(400));
        assert_eq!(compute_backoff(&config, 4), Duration::from_millis(800));
        // 1600ms would exceed the 1s cap.
        assert_eq!(compute_backoff(&config, 5), Duration::from_secs(1));
        assert_eq!(compute_backoff(&config, 20), Duration::from_secs(1));
    }
}
