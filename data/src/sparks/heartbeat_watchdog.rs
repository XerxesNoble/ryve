// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Liveness watchdog — scans active assignments and advances their
//! [`AssignmentLiveness`] when heartbeats stop arriving.
//!
//! Parent epic `ryve-cf05fd85` requires that silent Hands become an
//! observable state transition. A Hand emits a [`HeartbeatReceived`] event
//! at a fixed interval; this watchdog's tick compares every active
//! assignment's `last_heartbeat_at` against wall-clock `now` and decides:
//!
//! - `Healthy → AtRisk` when `age > 2 * heartbeat_interval_secs`.
//! - `AtRisk → Stuck` when `age > stuck_threshold_secs`.
//! - `AtRisk/Stuck → Healthy` when heartbeats resume and age drops back
//!   inside the healthy window. The AtRisk→Healthy edge keeps the derived
//!   state honest if a hand recovers; Stuck→Healthy is kept available so
//!   replays after a heartbeat storm converge, though the epic ultimately
//!   requires a Head/Director override to truly rescue a Stuck claim.
//!
//! Each transition is applied in a single sqlite transaction that both
//! advances the `assignments.liveness` column and appends a
//! [`LIVENESS_TRANSITIONED_EVENT_TYPE`] row to `event_outbox`. That means
//! the relay (and therefore any downstream subscriber like IRC) is guaranteed
//! to see the Stuck edge iff the DB was advanced. Projector-side, the
//! paired [`Event::LivenessTransitioned`] variant applies the same mutation
//! to [`AssignmentView::liveness`] so live and replayed state stay
//! byte-equal.
//!
//! The tick accepts `now: DateTime<Utc>` as an argument rather than calling
//! [`Utc::now`] internally — tests drive transitions with a fake clock by
//! advancing the `now` they pass in. `run` is the thin wrapper that calls
//! `tick` in a loop against `Utc::now()`.
//!
//! [`HeartbeatReceived`]: super::projector::Event::HeartbeatReceived
//! [`Event::LivenessTransitioned`]: super::projector::Event::LivenessTransitioned
//! [`AssignmentView::liveness`]: super::projector::AssignmentView::liveness

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use super::error::SparksError;
use super::projector::CURRENT_SCHEMA_VERSION;
use super::types::AssignmentLiveness;

/// `event_type` tag written to `event_outbox` for every liveness edge.
pub const LIVENESS_TRANSITIONED_EVENT_TYPE: &str = "LivenessTransitioned";

/// `actor_id` stamped on every watchdog-emitted outbox row. Stable so
/// relay subscribers can route watchdog traffic distinctly from hand /
/// merger / reviewer events.
pub const WATCHDOG_ACTOR: &str = "heartbeat-watchdog";

/// Default heartbeat cadence in seconds. The AtRisk transition fires at
/// 2x this value. Parent epic `ryve-cf05fd85`.
pub const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Default age at which an assignment is considered Stuck. Parent epic
/// `ryve-cf05fd85`.
pub const DEFAULT_STUCK_THRESHOLD_SECS: u64 = 300;

/// Default cadence at which [`run`] re-scans. Short enough to catch a
/// stopped Hand within one interval; long enough to keep the query load
/// on sqlite trivial.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Watchdog tuning knobs. The two age thresholds must satisfy
/// `stuck_threshold_secs > 2 * heartbeat_interval_secs` so the AtRisk
/// window is non-empty; [`Self::new`] enforces that.
#[derive(Debug, Clone, Copy)]
pub struct WatchdogConfig {
    /// Heartbeat cadence (seconds). `2 * heartbeat_interval_secs` is the
    /// Healthy→AtRisk threshold.
    pub heartbeat_interval_secs: u64,
    /// Heartbeat age (seconds) at which an assignment is declared Stuck.
    /// Must be strictly greater than `2 * heartbeat_interval_secs`.
    pub stuck_threshold_secs: u64,
    /// How long [`run`] sleeps between `tick` calls.
    pub poll_interval: Duration,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: DEFAULT_HEARTBEAT_INTERVAL_SECS,
            stuck_threshold_secs: DEFAULT_STUCK_THRESHOLD_SECS,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

impl WatchdogConfig {
    /// Build a config from explicit thresholds, validating that the Stuck
    /// boundary sits strictly past the AtRisk boundary. Returns `None` if
    /// the caller supplied an ordering that would collapse the AtRisk
    /// window (which would make transitions ambiguous).
    pub fn new(
        heartbeat_interval_secs: u64,
        stuck_threshold_secs: u64,
        poll_interval: Duration,
    ) -> Option<Self> {
        if stuck_threshold_secs <= heartbeat_interval_secs.saturating_mul(2) {
            return None;
        }
        Some(Self {
            heartbeat_interval_secs,
            stuck_threshold_secs,
            poll_interval,
        })
    }

    /// Threshold (seconds) at which Healthy advances to AtRisk.
    #[inline]
    pub fn at_risk_threshold_secs(&self) -> i64 {
        (self.heartbeat_interval_secs.saturating_mul(2)) as i64
    }

    /// Threshold (seconds) at which AtRisk advances to Stuck.
    #[inline]
    pub fn stuck_threshold_secs(&self) -> i64 {
        self.stuck_threshold_secs as i64
    }
}

/// Structured payload serialized into `event_outbox.payload` for every
/// liveness edge. Consumers (IRC bridge, projector replay, UI) pin the
/// same shape via this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivenessTransitionedPayload {
    pub assignment_id: String,
    pub spark_id: String,
    pub from_liveness: AssignmentLiveness,
    pub to_liveness: AssignmentLiveness,
    /// Wall clock the watchdog used when deciding the transition.
    pub observed_at: String,
    /// The `last_heartbeat_at` the watchdog read, or `None` if the
    /// assignment has never beaten — in that case the watchdog falls
    /// back to `assigned_at` for age calculations.
    pub last_heartbeat_at: Option<String>,
    /// Age in seconds the watchdog attributed to the assignment (computed
    /// as `observed_at - last_heartbeat_at`, falling back to
    /// `assigned_at` when no heartbeat has been recorded).
    pub age_secs: i64,
}

/// One active assignment the watchdog needs to evaluate. Queried from
/// `assignments` with the subset of columns required for the liveness
/// decision plus the outbox row's `assignment_id` / `spark_id` fields.
#[derive(Debug, Clone, sqlx::FromRow)]
struct ActiveAssignment {
    assignment_id: String,
    spark_id: String,
    assigned_at: Option<String>,
    last_heartbeat_at: Option<String>,
    liveness: String,
}

/// Summary of one [`tick`] pass. Exposed so tests can assert exact counts
/// of advanced / stuck / unchanged assignments without inspecting the DB.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickOutcome {
    /// Rows fetched from `assignments` (status = 'active').
    pub scanned: usize,
    /// Assignments the watchdog transitioned on this pass (each produces
    /// a [`LIVENESS_TRANSITIONED_EVENT_TYPE`] outbox row).
    pub transitioned: usize,
    /// Assignments transitioned *to* Stuck on this pass. Subset of
    /// `transitioned`.
    pub became_stuck: usize,
    /// Assignments transitioned *to* AtRisk on this pass. Subset of
    /// `transitioned`.
    pub became_at_risk: usize,
}

/// Run one watchdog pass at wall-clock `now`. Every active assignment is
/// evaluated; any that crosses a liveness threshold is updated and emits
/// a canonical outbox event in the same transaction.
pub async fn tick(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    config: &WatchdogConfig,
) -> Result<TickOutcome, SparksError> {
    let assignments = fetch_active(pool).await?;
    let mut outcome = TickOutcome {
        scanned: assignments.len(),
        ..TickOutcome::default()
    };

    for row in assignments {
        let Some(current) = AssignmentLiveness::from_str(&row.liveness) else {
            // An unknown liveness value is a schema-drift bug, not a
            // transient one — skip rather than bail so a single bad row
            // doesn't starve the rest of the workshop.
            continue;
        };

        let age = age_secs(&row, now);
        let target = classify(age, config);

        if target == current {
            continue;
        }

        apply_transition(pool, &row, current, target, now, age).await?;
        outcome.transitioned += 1;
        match target {
            AssignmentLiveness::Stuck => outcome.became_stuck += 1,
            AssignmentLiveness::AtRisk => outcome.became_at_risk += 1,
            AssignmentLiveness::Healthy => {}
        }
    }

    Ok(outcome)
}

/// Spawn the watchdog on a tokio task. Loops forever at `config.poll_interval`.
/// Intended to be fired from app startup; ticks use `Utc::now()` so the
/// caller does not need to thread a clock through the rest of the app.
pub async fn run(pool: SqlitePool, config: WatchdogConfig) -> Result<(), SparksError> {
    loop {
        tick(&pool, Utc::now(), &config).await?;
        tokio::time::sleep(config.poll_interval).await;
    }
}

/// Decide the liveness that `age` (seconds since last heartbeat) maps to
/// under `config`. Pure; exposed at crate-visibility so tests can probe
/// the classification without touching the DB.
pub(crate) fn classify(age_secs: i64, config: &WatchdogConfig) -> AssignmentLiveness {
    if age_secs > config.stuck_threshold_secs() {
        AssignmentLiveness::Stuck
    } else if age_secs > config.at_risk_threshold_secs() {
        AssignmentLiveness::AtRisk
    } else {
        AssignmentLiveness::Healthy
    }
}

async fn fetch_active(pool: &SqlitePool) -> Result<Vec<ActiveAssignment>, SparksError> {
    let rows = sqlx::query_as::<_, ActiveAssignment>(
        "SELECT assignment_id, spark_id, assigned_at, last_heartbeat_at, liveness \
         FROM assignments WHERE status = 'active'",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

fn age_secs(row: &ActiveAssignment, now: DateTime<Utc>) -> i64 {
    // Prefer the heartbeat timestamp; fall back to assigned_at so a Hand
    // that was assigned but has never beaten still ages into AtRisk and
    // eventually Stuck. An assignment with neither timestamp is treated
    // as age 0 — effectively Healthy — so the watchdog never transitions
    // on a malformed row.
    let anchor = row
        .last_heartbeat_at
        .as_deref()
        .or(row.assigned_at.as_deref());
    match anchor.and_then(parse_rfc3339_ok) {
        Some(t) => (now - t).num_seconds(),
        None => 0,
    }
}

fn parse_rfc3339_ok(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

async fn apply_transition(
    pool: &SqlitePool,
    row: &ActiveAssignment,
    from: AssignmentLiveness,
    to: AssignmentLiveness,
    now: DateTime<Utc>,
    age_secs: i64,
) -> Result<(), SparksError> {
    let now_str = now.to_rfc3339();
    let event_id = format!("evt-{}", Uuid::new_v4());

    let payload = LivenessTransitionedPayload {
        assignment_id: row.assignment_id.clone(),
        spark_id: row.spark_id.clone(),
        from_liveness: from,
        to_liveness: to,
        observed_at: now_str.clone(),
        last_heartbeat_at: row.last_heartbeat_at.clone(),
        age_secs,
    };
    let payload_json =
        serde_json::to_string(&payload).map_err(|e| SparksError::Serialization(e.to_string()))?;

    let mut tx = pool.begin().await?;

    // Conditional UPDATE — if another tick has already advanced this row
    // past `from`, the match count is zero and we roll back without
    // emitting a duplicate event. This matches the "claim before
    // emitting" discipline used by watch_runner [sp-934807b1].
    let result = sqlx::query(
        "UPDATE assignments SET liveness = ? \
         WHERE assignment_id = ? AND status = 'active' AND liveness = ?",
    )
    .bind(to.as_str())
    .bind(&row.assignment_id)
    .bind(from.as_str())
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO event_outbox \
         (event_id, schema_version, timestamp, assignment_id, actor_id, event_type, payload) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event_id)
    .bind(CURRENT_SCHEMA_VERSION as i64)
    .bind(&now_str)
    .bind(&row.assignment_id)
    .bind(WATCHDOG_ACTOR)
    .bind(LIVENESS_TRANSITIONED_EVENT_TYPE)
    .bind(&payload_json)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_walks_healthy_at_risk_stuck() {
        let config = WatchdogConfig::new(30, 300, Duration::from_secs(5)).unwrap();
        // Boundaries are strict ">", matching the acceptance criteria.
        assert_eq!(classify(0, &config), AssignmentLiveness::Healthy);
        assert_eq!(classify(60, &config), AssignmentLiveness::Healthy);
        assert_eq!(classify(61, &config), AssignmentLiveness::AtRisk);
        assert_eq!(classify(300, &config), AssignmentLiveness::AtRisk);
        assert_eq!(classify(301, &config), AssignmentLiveness::Stuck);
        assert_eq!(classify(10_000, &config), AssignmentLiveness::Stuck);
    }

    #[test]
    fn config_new_rejects_collapsed_at_risk_window() {
        // Stuck threshold must be strictly > 2x heartbeat interval, else
        // the AtRisk window collapses to zero and the state machine is
        // ambiguous.
        assert!(WatchdogConfig::new(30, 60, Duration::from_secs(5)).is_none());
        assert!(WatchdogConfig::new(30, 59, Duration::from_secs(5)).is_none());
        assert!(WatchdogConfig::new(30, 61, Duration::from_secs(5)).is_some());
    }

    #[test]
    fn liveness_payload_round_trips_through_json() {
        let payload = LivenessTransitionedPayload {
            assignment_id: "asgn-xyz".into(),
            spark_id: "ryve-abc".into(),
            from_liveness: AssignmentLiveness::Healthy,
            to_liveness: AssignmentLiveness::AtRisk,
            observed_at: "2026-04-17T12:00:00+00:00".into(),
            last_heartbeat_at: Some("2026-04-17T11:58:00+00:00".into()),
            age_secs: 120,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: LivenessTransitionedPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }
}
