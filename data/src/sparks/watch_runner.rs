// SPDX-License-Identifier: AGPL-3.0-or-later

//! Core tick logic for the durable watch scheduler.
//!
//! Spark ryve-6ab1980c [sp-ee3f5c74]: given a wall-clock `now`, [`tick`]
//! finds every due watch and fires it exactly once. Each firing does two
//! things in a **single transaction**:
//!
//! 1. appends a `WatchFired` row to `event_outbox` (pattern from epic
//!    ryve-3575a5fe), and
//! 2. advances the watch's `last_fired_at` / `next_fire_at` — or marks
//!    the watch `completed` if its stop condition is now satisfied.
//!
//! Because the two writes share one tx, a crash between them cannot leave
//! the event logged without the schedule advancing (or vice versa), so
//! the "exactly-once per `(watch_id, scheduled_fire_at)` slot" invariant
//! holds across process restarts.
//!
//! Missed-tick catch-up is bounded to one event per watch per tick: on
//! restart after downtime the runner fires once for the most recent due
//! slot and advances `next_fire_at` past `now` in the same tx, rather
//! than replaying every slot in the gap.
//!
//! The app-layer wrapper that drives [`tick`] every N seconds from a
//! tokio task lives in `src/watch_runner.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use super::error::SparksError;
use super::types::*;
use super::watch_repo;

/// Canonical `schema_version` stamped on every `WatchFired` outbox row.
/// Bumping this is the migration boundary for downstream consumers.
pub const WATCH_FIRED_SCHEMA_VERSION: i64 = 1;

/// `event_type` tag used for `WatchFired` rows in `event_outbox`.
pub const WATCH_FIRED_EVENT_TYPE: &str = "WatchFired";

/// `actor_id` stamped on every emitted `WatchFired` event. A stable
/// identifier so relay subscribers can route watch traffic distinctly.
pub const WATCH_RUNNER_ACTOR: &str = "watch-runner";

/// Structured payload written to `event_outbox.payload` for a
/// `WatchFired` event. Kept here so producers and consumers pin the same
/// shape at a known [`WATCH_FIRED_SCHEMA_VERSION`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchFiredPayload {
    pub watch_id: String,
    pub target_spark_id: String,
    pub intent_label: String,
    /// The `next_fire_at` slot this firing is *for* — NOT the wall-clock
    /// time at which the runner emitted it. This is the deduplication
    /// key across restarts: a watch only fires once per
    /// `(watch_id, scheduled_fire_at)` pair.
    pub scheduled_fire_at: String,
    /// Wall-clock at which the runner actually emitted the event. Useful
    /// for observing downtime / catch-up latency.
    pub fired_at: String,
    pub cadence: WatchCadence,
    /// True when the firing also transitioned the watch to `completed`
    /// because its stop condition was satisfied.
    pub stop_condition_satisfied: bool,
}

/// Summary of one [`tick`] pass. Returned primarily so tests can make
/// exact assertions about what happened; the runtime wrapper only logs it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickOutcome {
    /// Watches that emitted a `WatchFired` event on this pass.
    pub fired: usize,
    /// Watches transitioned to `completed` because their stop condition
    /// was satisfied. Every entry here is also counted in [`Self::fired`].
    pub completed: usize,
    /// Watches observed as due but not fired (e.g. unsupported cron
    /// cadence or a corrupt persisted value). Their `next_fire_at` is
    /// advanced to a safe future instant so the loop does not tight-loop.
    pub skipped: usize,
}

/// Run one pass of the scheduler against `pool` at wall-clock `now`.
///
/// Every due watch (`status = 'active'` AND `next_fire_at <= now`) is
/// fired in its own transaction that inserts the outbox row and advances
/// (or completes) the schedule atomically.
pub async fn tick(pool: &SqlitePool, now: DateTime<Utc>) -> Result<TickOutcome, SparksError> {
    let now_str = now.to_rfc3339();
    let due = watch_repo::due_at(pool, &now_str).await?;

    let mut outcome = TickOutcome::default();
    for watch in due {
        match fire_one(pool, &watch, now).await? {
            FireOutcome::Fired => outcome.fired += 1,
            FireOutcome::FiredAndCompleted => {
                outcome.fired += 1;
                outcome.completed += 1;
            }
            FireOutcome::Skipped => outcome.skipped += 1,
        }
    }
    Ok(outcome)
}

enum FireOutcome {
    Fired,
    FiredAndCompleted,
    Skipped,
}

async fn fire_one(
    pool: &SqlitePool,
    watch: &Watch,
    now: DateTime<Utc>,
) -> Result<FireOutcome, SparksError> {
    let Some(cadence) = watch.parsed_cadence() else {
        // Corrupt cadence — advance `next_fire_at` so we don't hot-loop
        // on this watch on every tick, and skip firing.
        advance_next_fire_only(pool, &watch.id, now + chrono::Duration::seconds(60)).await?;
        return Ok(FireOutcome::Skipped);
    };

    let scheduled = parse_rfc3339(&watch.next_fire_at)?;
    let next_fire_at = next_slot_after(&cadence, scheduled, now);

    let now_str = now.to_rfc3339();
    let event_id = format!("evt-{}", Uuid::new_v4());

    let mut tx = pool.begin().await?;

    let stop_satisfied = evaluate_stop_condition_in_tx(&mut tx, watch).await?;

    let payload = WatchFiredPayload {
        watch_id: watch.id.clone(),
        target_spark_id: watch.target_spark_id.clone(),
        intent_label: watch.intent_label.clone(),
        scheduled_fire_at: watch.next_fire_at.clone(),
        fired_at: now_str.clone(),
        cadence: cadence.clone(),
        stop_condition_satisfied: stop_satisfied,
    };
    let payload_json =
        serde_json::to_string(&payload).map_err(|e| SparksError::Serialization(e.to_string()))?;

    sqlx::query(
        "INSERT INTO event_outbox \
         (event_id, schema_version, timestamp, assignment_id, actor_id, event_type, payload) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event_id)
    .bind(WATCH_FIRED_SCHEMA_VERSION)
    .bind(&now_str)
    .bind(&watch.id)
    .bind(WATCH_RUNNER_ACTOR)
    .bind(WATCH_FIRED_EVENT_TYPE)
    .bind(&payload_json)
    .execute(&mut *tx)
    .await?;

    if stop_satisfied {
        watch_repo::mark_completed_in_tx(&mut tx, &watch.id, &now_str).await?;
    } else {
        watch_repo::mark_fired_in_tx(&mut tx, &watch.id, &now_str, &next_fire_at.to_rfc3339())
            .await?;
    }

    tx.commit().await?;

    Ok(if stop_satisfied {
        FireOutcome::FiredAndCompleted
    } else {
        FireOutcome::Fired
    })
}

async fn advance_next_fire_only(
    pool: &SqlitePool,
    watch_id: &str,
    next: DateTime<Utc>,
) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();
    sqlx::query("UPDATE watches SET next_fire_at = ?, updated_at = ? WHERE id = ?")
        .bind(next.to_rfc3339())
        .bind(&now)
        .bind(watch_id)
        .execute(pool)
        .await?;
    Ok(())
}

async fn evaluate_stop_condition_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    watch: &Watch,
) -> Result<bool, SparksError> {
    let Some(stop) = watch.parsed_stop_condition() else {
        return Ok(false);
    };
    match stop {
        WatchStopCondition::Never => Ok(false),
        WatchStopCondition::UntilSparkStatus { spark_id, status } => {
            let row: Option<String> = sqlx::query_scalar("SELECT status FROM sparks WHERE id = ?")
                .bind(&spark_id)
                .fetch_optional(&mut **tx)
                .await?;
            Ok(row.map(|s| s == status).unwrap_or(false))
        }
        // UntilEventType lookup needs its own event-log conventions and is
        // out of scope for this spark. Treat as "never" so the watch keeps
        // firing; a downstream sibling can tighten this.
        WatchStopCondition::UntilEventType { .. } => Ok(false),
    }
}

/// Compute the next fire instant on `cadence`'s schedule that is strictly
/// greater than `now`, starting from `scheduled` (the slot we are firing
/// for).
///
/// For [`WatchCadence::Interval`] this skips any slots already passed, so
/// a restart after downtime fires once and resumes cadence rather than
/// replaying the whole backlog.
fn next_slot_after(
    cadence: &WatchCadence,
    scheduled: DateTime<Utc>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    match cadence {
        WatchCadence::Interval { secs } => {
            let step = chrono::Duration::seconds((*secs).max(1) as i64);
            let mut next = scheduled + step;
            while next <= now {
                next += step;
            }
            next
        }
        WatchCadence::Cron { .. } => {
            // Full cron parsing is out of scope for this spark. Advance
            // conservatively past `now` so the scheduler does not
            // tight-loop on an unsupported cadence — a downstream sibling
            // can swap in a real cron evaluator.
            now + chrono::Duration::seconds(60)
        }
    }
}

fn parse_rfc3339(s: &str) -> Result<DateTime<Utc>, SparksError> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| SparksError::Serialization(format!("rfc3339 parse {s:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_next_slot_skips_past_now() {
        let scheduled: DateTime<Utc> = "2026-04-17T00:00:00+00:00".parse().unwrap();
        let cadence = WatchCadence::Interval { secs: 10 };

        // now is 25s past scheduled — three slots elapsed, so next is the
        // fourth (30s after scheduled).
        let now: DateTime<Utc> = "2026-04-17T00:00:25+00:00".parse().unwrap();
        let next = next_slot_after(&cadence, scheduled, now);
        assert_eq!(next.to_rfc3339(), "2026-04-17T00:00:30+00:00");

        // now exactly on a slot boundary — must advance strictly past it.
        let now: DateTime<Utc> = "2026-04-17T00:00:20+00:00".parse().unwrap();
        let next = next_slot_after(&cadence, scheduled, now);
        assert_eq!(next.to_rfc3339(), "2026-04-17T00:00:30+00:00");

        // now before the first slot — first slot wins.
        let now: DateTime<Utc> = "2026-04-16T23:59:59+00:00".parse().unwrap();
        let next = next_slot_after(&cadence, scheduled, now);
        assert_eq!(next.to_rfc3339(), "2026-04-17T00:00:10+00:00");
    }

    #[test]
    fn interval_zero_secs_is_clamped_to_one() {
        let scheduled: DateTime<Utc> = "2026-04-17T00:00:00+00:00".parse().unwrap();
        let now: DateTime<Utc> = "2026-04-17T00:00:05+00:00".parse().unwrap();
        let next = next_slot_after(&WatchCadence::Interval { secs: 0 }, scheduled, now);
        // Clamped step of 1s; next must be strictly past `now`.
        assert!(next > now);
    }

    #[test]
    fn cron_falls_back_to_now_plus_60s() {
        let scheduled: DateTime<Utc> = "2026-04-17T00:00:00+00:00".parse().unwrap();
        let now: DateTime<Utc> = "2026-04-17T00:05:00+00:00".parse().unwrap();
        let next = next_slot_after(
            &WatchCadence::Cron {
                expr: "*/5 * * * *".into(),
            },
            scheduled,
            now,
        );
        assert_eq!(next, now + chrono::Duration::seconds(60));
    }

    #[test]
    fn watch_fired_payload_round_trips_through_json() {
        let payload = WatchFiredPayload {
            watch_id: "watch-abc".into(),
            target_spark_id: "ryve-x".into(),
            intent_label: "poll".into(),
            scheduled_fire_at: "2026-04-17T00:00:00+00:00".into(),
            fired_at: "2026-04-17T00:00:05+00:00".into(),
            cadence: WatchCadence::Interval { secs: 30 },
            stop_condition_satisfied: false,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: WatchFiredPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }
}
