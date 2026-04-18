// SPDX-License-Identifier: AGPL-3.0-or-later

//! Orphan-detection scanner: enforce the no-invisible-work invariant.
//!
//! The parent epic (`ryve-73e42cac`) requires every `assignments` row in
//! any phase `≥ AwaitingReview` to be linked to a GitHub artifact
//! (head branch + PR number). Any assignment that reaches
//! `AwaitingReview`/`Approved`/`Rejected`/`InRepair`/`ReadyForMerge`/
//! `Merged` without that link is "invisible" to GitHub reviewers and the
//! mirror cannot drive its next transition — drift between Ryve state
//! and GitHub goes undetected.
//!
//! [`run_orphan_scan`] scans the workgraph for those rows and appends a
//! warning row to `event_outbox` for each one. The warning flows through
//! the existing outbox relay so IRC, UI, and any other subscriber see it
//! alongside normal mirror traffic.
//!
//! # Idempotency
//!
//! The scan is safe to re-run. We reuse the existing `github_events_seen`
//! dedup table (from migration 019) with a synthetic
//! `github_event_id` shaped as
//! `orphan-scan:{assignment_id}:{bucket_epoch}`, where `bucket_epoch` is
//! the wall clock floored to a debounce window (default 1h). The first
//! scan of a given assignment inside a window emits a warning and marks
//! the synthetic id seen; repeat scans inside the same window short-
//! circuit. After the window ticks over, the synthetic id changes and a
//! fresh warning is emitted — so a persistent orphan re-pages instead of
//! being silenced forever.
//!
//! Choosing `github_events_seen` over a dedicated `assignment_warnings`
//! table is the lighter-weight option: no new schema, no new repo, and
//! it keeps all mirror-side dedup in one place so oncall learns one
//! table, not two.

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{Sqlite, Transaction};
use uuid::Uuid;

use super::applier::{APPLIER_SCHEMA_VERSION, ApplyError, GithubEventsSeenRepo};
use crate::sparks::types::AssignmentPhase;

/// `event_outbox.event_type` stamped on every warning this module emits.
/// Distinct from [`super::applier::EVT_ORPHAN_EVENT_WARNING`] (that one
/// fires when a GitHub event cannot be routed to an Assignment; this one
/// fires when an Assignment has no GitHub artifact).
pub const EVT_ORPHAN_ASSIGNMENT_WARNING: &str = "github.orphan_assignment_warning";

/// `actor_id` stamped on orphan-scan outbox rows and on their
/// `github_events_seen` dedup markers. Stable string so downstream
/// consumers can route scanner traffic distinctly from webhook traffic.
pub const ORPHAN_SCAN_ACTOR: &str = "github-mirror-scanner";

/// `github_events_seen.event_type` stamped on the synthetic dedup rows.
/// Distinct from any real GitHub webhook `kind` so dedup cleanup / audit
/// can tell scanner traffic apart.
pub const ORPHAN_SCAN_EVENT_TYPE: &str = "orphan_assignment_scan";

/// Default debounce bucket size — one hour. A persistent orphan re-pages
/// at most once per hour, which is noisy enough that oncall notices and
/// quiet enough to avoid flooding IRC on every scan tick.
pub const DEFAULT_DEBOUNCE_SECONDS: i64 = 3600;

/// Result of one [`run_orphan_scan`] pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OrphanScanOutcome {
    /// Rows observed as orphan candidates (matched the phase+null-artifact
    /// predicate). Equals `warned + debounced`.
    pub scanned: usize,
    /// Orphans that produced a fresh `EVT_ORPHAN_ASSIGNMENT_WARNING` row
    /// in this pass.
    pub warned: usize,
    /// Orphans whose synthetic dedup marker was already present (same
    /// debounce bucket) and therefore skipped.
    pub debounced: usize,
}

/// Pure query predicate: `true` iff an assignment in `phase` with
/// `has_artifact = (github_artifact_pr_number IS NOT NULL)` is an orphan
/// candidate the scanner should warn about.
///
/// Kept separate from the SQL so the phase table can be exercised
/// exhaustively without a database. The SQL query in [`run_orphan_scan`]
/// matches this predicate 1:1.
pub fn is_orphan_candidate(phase: AssignmentPhase, has_artifact: bool) -> bool {
    if has_artifact {
        return false;
    }
    matches!(
        phase,
        AssignmentPhase::AwaitingReview
            | AssignmentPhase::Approved
            | AssignmentPhase::Rejected
            | AssignmentPhase::InRepair
            | AssignmentPhase::ReadyForMerge
            | AssignmentPhase::Merged
    )
}

/// Phase strings the SQL predicate selects. Constructed from
/// [`is_orphan_candidate`] so the two stay aligned; any new phase added
/// to [`AssignmentPhase`] automatically flows through the predicate and
/// is included here iff it is orphan-reportable.
fn orphan_reportable_phases() -> Vec<&'static str> {
    AssignmentPhase::ALL
        .iter()
        .filter(|p| is_orphan_candidate(**p, false))
        .map(|p| p.as_str())
        .collect()
}

/// One orphan row picked up by the phase-filtered query. Held briefly in
/// memory between selection and per-row emission so the transaction
/// doesn't keep a cursor open across outbox writes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OrphanRow {
    /// External text id from `assignments.assignment_id` — the same
    /// identifier the rest of the mirror (and outbox consumers) carry.
    /// Note this is NOT the internal integer PK `assignments.id`.
    assignment_id: String,
    spark_id: String,
    phase: AssignmentPhase,
}

/// Run a full orphan-detection scan with default wall-clock and debounce.
///
/// Caller owns the transaction so the scan + its outbox rows commit
/// atomically with whatever else the caller is doing (e.g. a tick of the
/// mirror poller).
pub async fn run_orphan_scan(
    tx: &mut Transaction<'_, Sqlite>,
) -> Result<OrphanScanOutcome, ApplyError> {
    run_orphan_scan_with(
        tx,
        Utc::now(),
        DEFAULT_DEBOUNCE_SECONDS,
        &GithubEventsSeenRepo::new(),
    )
    .await
}

/// [`run_orphan_scan`] with explicit wall-clock and debounce window —
/// exposed so tests can drive both knobs deterministically.
///
/// `debounce_seconds` values `<= 0` are treated as `1` so the bucket
/// computation cannot divide by zero or produce negative strides.
pub async fn run_orphan_scan_with(
    tx: &mut Transaction<'_, Sqlite>,
    now: DateTime<Utc>,
    debounce_seconds: i64,
    seen: &GithubEventsSeenRepo,
) -> Result<OrphanScanOutcome, ApplyError> {
    let debounce = debounce_seconds.max(1);
    let bucket = now.timestamp().div_euclid(debounce) * debounce;

    let phases = orphan_reportable_phases();
    let placeholders = std::iter::repeat_n("?", phases.len())
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "SELECT assignment_id, spark_id, assignment_phase \
         FROM assignments \
         WHERE assignment_phase IN ({placeholders}) \
           AND github_artifact_pr_number IS NULL \
         ORDER BY assignment_id ASC"
    );

    let mut q = sqlx::query_as::<_, (String, String, String)>(&query);
    for phase_str in &phases {
        q = q.bind(*phase_str);
    }
    let rows = q.fetch_all(&mut **tx).await?;

    let mut outcome = OrphanScanOutcome::default();
    for (assignment_id, spark_id, phase_str) in rows {
        // An unknown phase string would have been filtered out by the
        // IN (..) clause, so this should always succeed; if it doesn't,
        // the row is malformed and we skip it rather than crash the
        // whole scan.
        let Some(phase) = AssignmentPhase::from_str(&phase_str) else {
            continue;
        };
        outcome.scanned += 1;

        let synthetic_key = synthetic_dedup_key(&assignment_id, bucket);
        if seen.is_seen(tx, &synthetic_key).await? {
            outcome.debounced += 1;
            continue;
        }

        let row = OrphanRow {
            assignment_id,
            spark_id,
            phase,
        };
        emit_orphan_assignment_warning(tx, &row, &synthetic_key, bucket, now).await?;
        seen.mark_seen(tx, &synthetic_key, ORPHAN_SCAN_EVENT_TYPE)
            .await?;
        outcome.warned += 1;
    }

    Ok(outcome)
}

fn synthetic_dedup_key(assignment_id: &str, bucket_epoch: i64) -> String {
    format!("orphan-scan:{assignment_id}:{bucket_epoch}")
}

async fn emit_orphan_assignment_warning(
    tx: &mut Transaction<'_, Sqlite>,
    row: &OrphanRow,
    synthetic_key: &str,
    bucket_epoch: i64,
    now: DateTime<Utc>,
) -> Result<(), ApplyError> {
    let payload = json!({
        "assignment_id": row.assignment_id,
        "spark_id": row.spark_id,
        "phase": row.phase.as_str(),
        "debounce_key": synthetic_key,
        "debounce_bucket_epoch": bucket_epoch,
        "reason": "assignment reached a reviewable phase without a github_artifact",
    });
    let payload_str =
        serde_json::to_string(&payload).map_err(|e| ApplyError::Serialization(e.to_string()))?;

    let event_id = Uuid::new_v4().to_string();
    let now_str = now.to_rfc3339();
    sqlx::query(
        "INSERT INTO event_outbox \
         (event_id, schema_version, timestamp, assignment_id, actor_id, event_type, payload) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event_id)
    .bind(APPLIER_SCHEMA_VERSION)
    .bind(&now_str)
    .bind(&row.assignment_id)
    .bind(ORPHAN_SCAN_ACTOR)
    .bind(EVT_ORPHAN_ASSIGNMENT_WARNING)
    .bind(&payload_str)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pure predicate: one row per phase × artifact-present combo ─────

    #[test]
    fn predicate_covers_every_phase_without_artifact() {
        let cases: &[(AssignmentPhase, bool)] = &[
            (AssignmentPhase::Assigned, false),
            (AssignmentPhase::InProgress, false),
            (AssignmentPhase::AwaitingReview, true),
            (AssignmentPhase::Approved, true),
            (AssignmentPhase::Rejected, true),
            (AssignmentPhase::InRepair, true),
            (AssignmentPhase::ReadyForMerge, true),
            (AssignmentPhase::Merged, true),
        ];
        for (phase, expected) in cases {
            assert_eq!(
                is_orphan_candidate(*phase, false),
                *expected,
                "phase {:?} without artifact must be orphan={}",
                phase,
                expected,
            );
        }
        // Safety-net: the case table must walk every phase exactly once.
        assert_eq!(
            cases.len(),
            AssignmentPhase::ALL.len(),
            "table-driven case list must cover every AssignmentPhase variant",
        );
    }

    #[test]
    fn predicate_never_flags_when_artifact_is_present() {
        for phase in AssignmentPhase::ALL {
            assert!(
                !is_orphan_candidate(*phase, true),
                "phase {phase:?} with artifact must NOT be orphan",
            );
        }
    }

    #[test]
    fn orphan_reportable_phases_matches_predicate() {
        // The SQL IN-clause phase list is derived from the predicate.
        // Asserting equality here keeps the two in lock-step so a future
        // AssignmentPhase variant cannot be reachable by the predicate
        // but missing from the query.
        let expected: Vec<&'static str> = AssignmentPhase::ALL
            .iter()
            .filter(|p| is_orphan_candidate(**p, false))
            .map(|p| p.as_str())
            .collect();
        assert_eq!(orphan_reportable_phases(), expected);
    }

    #[test]
    fn synthetic_key_includes_assignment_and_bucket() {
        let k = synthetic_dedup_key("asgn-1", 123_456);
        assert_eq!(k, "orphan-scan:asgn-1:123456");
    }

    #[test]
    fn synthetic_key_differs_across_buckets_for_same_assignment() {
        let a = synthetic_dedup_key("asgn-1", 0);
        let b = synthetic_dedup_key("asgn-1", 3600);
        assert_ne!(a, b, "different buckets must produce different dedup keys");
    }
}
