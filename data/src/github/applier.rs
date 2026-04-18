// SPDX-License-Identifier: AGPL-3.0-or-later

//! Canonical GitHub events → Assignment state transitions.
//!
//! The applier is the glue between the pure translator
//! ([`super::translator::translate`]) and the Assignment state machine
//! ([`crate::sparks::transition`]). Every ingestion path — webhook HTTP,
//! REST poller, manual replay — lands here after translation, and the
//! applier drives the visible side-effects: phase transitions through
//! the existing validator, one outbox row per durable effect, and
//! `github_artifact_*` population on first PR opened.
//!
//! # Contract
//!
//! - **Idempotent by `github_event_id`.** The first thing [`apply`] does
//!   is consult [`GithubEventsSeenRepo`]. Any duplicate returns
//!   [`AppliedOutcome::Duplicate`] and writes nothing — retried webhook
//!   deliveries and poller re-runs are free.
//! - **Validator is the only path to `assignment_phase`.** The applier
//!   never UPDATEs that column directly; every transition is routed
//!   through [`validate_transition`] and a single SQL UPDATE that mirrors
//!   what `transition::transition_assignment_phase` writes in its own
//!   transaction. If the validator refuses the transition, [`apply`]
//!   emits a warning row to `event_outbox` and returns
//!   [`ApplyError::Transition`].
//! - **Caller owns the transaction.** The applier takes `&mut Tx` so a
//!   batch of canonical events can be applied atomically. Even on error
//!   the Tx carries useful rows (the warning and the `github_events_seen`
//!   mark), so callers that want to keep the audit trail must commit
//!   before surfacing the error.
//!
//! # Outbox event types
//!
//! The applier is the single producer of these `event_outbox.event_type`
//! tags — downstream consumers (IRC bridge, UI projector) may pattern
//! match on them.
//!
//! | Tag                                      | When                                    |
//! |------------------------------------------|-----------------------------------------|
//! | [`EVT_PHASE_TRANSITIONED`]               | A legal phase transition was persisted. |
//! | [`EVT_ARTIFACT_RECORDED`]                | First PR opened for an assignment.      |
//! | [`EVT_ILLEGAL_TRANSITION_WARNING`]       | Validator refused the transition.       |
//! | [`EVT_ORPHAN_EVENT_WARNING`]             | No Assignment matched the event.        |

use chrono::Utc;
use serde_json::json;
use sqlx::{Sqlite, Transaction};
use uuid::Uuid;

use super::types::CanonicalGitHubEvent;
use crate::sparks::error::TransitionError;
use crate::sparks::transition::validate_transition;
use crate::sparks::types::{Assignment, AssignmentPhase, TransitionActorRole};

/// Schema version stamped on every outbox row the applier writes.
/// Bump when the payload shape changes.
pub const APPLIER_SCHEMA_VERSION: i64 = 1;

/// `event_outbox.event_type` for a successful phase transition.
pub const EVT_PHASE_TRANSITIONED: &str = "github.assignment_phase_transitioned";

/// `event_outbox.event_type` for the first PR-opened → artifact record.
pub const EVT_ARTIFACT_RECORDED: &str = "github.artifact_recorded";

/// `event_outbox.event_type` for a validator rejection. Carries the
/// attempted transition and the canonical error so oncall can replay.
pub const EVT_ILLEGAL_TRANSITION_WARNING: &str = "github.illegal_transition_warning";

/// `event_outbox.event_type` for events that could not be routed to any
/// Assignment row (PR number not mirrored, head branch unknown, …).
pub const EVT_ORPHAN_EVENT_WARNING: &str = "github.orphan_event_warning";

/// Actor string stamped on outbox rows that originate from the mirror.
/// Downstream consumers use this to distinguish applier-driven events
/// from Hand-driven ones.
const APPLIER_ACTOR: &str = "github-mirror";

/// Idempotency keeper for the GitHub mirror. Every mutation goes through
/// [`is_seen`]/[`mark_seen`] so a retried webhook delivery is a no-op.
///
/// Implemented as a zero-sized handle — the concrete state lives in the
/// `github_events_seen` table, reached via the caller's `&mut Tx`. Kept
/// as a named type so the applier's public signature makes the
/// idempotency requirement explicit instead of hiding behind a raw SQL
/// call.
///
/// [`is_seen`]: GithubEventsSeenRepo::is_seen
/// [`mark_seen`]: GithubEventsSeenRepo::mark_seen
#[derive(Debug, Default, Clone, Copy)]
pub struct GithubEventsSeenRepo;

impl GithubEventsSeenRepo {
    pub const fn new() -> Self {
        Self
    }

    /// Return `true` if `github_event_id` has already been applied.
    pub async fn is_seen(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        github_event_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let row: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM github_events_seen WHERE github_event_id = ?")
                .bind(github_event_id)
                .fetch_optional(&mut **tx)
                .await?;
        Ok(row.is_some())
    }

    /// Record that `github_event_id` has been applied. Uses
    /// `INSERT OR IGNORE` so a concurrent marker from a sibling
    /// transaction cannot race into a constraint error.
    pub async fn mark_seen(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        github_event_id: &str,
        event_type: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT OR IGNORE INTO github_events_seen (github_event_id, event_type, ingested_at) \
             VALUES (?, ?, ?)",
        )
        .bind(github_event_id)
        .bind(event_type)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

/// Errors [`apply`] can surface to the caller.
///
/// A [`Transition`] variant means the validator refused the attempted
/// transition; the applier has already written a warning row to
/// `event_outbox` and marked the event seen, so the caller MUST commit
/// the transaction to keep the audit trail.
///
/// [`Transition`]: ApplyError::Transition
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("transition refused by validator: {0}")]
    Transition(#[from] TransitionError),

    #[error("serialization error: {0}")]
    Serialization(String),
}

/// The effect [`apply`] had on the workgraph for one canonical event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedOutcome {
    /// `github_event_id` was already recorded. No rows written.
    Duplicate,

    /// The event is canonical but intentionally non-state-changing for
    /// the mirror (edits, comments, CI success, unmerged close). Marked
    /// seen; no `event_outbox` row.
    Ignored,

    /// No Assignment matched the event's PR / branch. A warning row was
    /// written to `event_outbox`.
    NoAssignment { pr_number: i64 },

    /// A `PrOpened` event populated `github_artifact_*` on an assignment
    /// that previously had no artifact. Emits [`EVT_ARTIFACT_RECORDED`].
    ArtifactRecorded { assignment_id: i64, pr_number: i64 },

    /// The validator accepted the transition and the update was applied.
    /// Emits [`EVT_PHASE_TRANSITIONED`].
    Transitioned {
        assignment_id: i64,
        from: AssignmentPhase,
        to: AssignmentPhase,
    },
}

/// Apply a canonical GitHub event to the workgraph.
///
/// See the module docs for the full contract. Short version: idempotent
/// on `github_event_id`, never UPDATEs `assignment_phase` directly, one
/// outbox row per durable effect. On validator rejection the Tx carries
/// a warning row and a seen-marker; commit to persist them.
pub async fn apply(
    tx: &mut Transaction<'_, Sqlite>,
    github_event_id: &str,
    event: &CanonicalGitHubEvent,
    seen: &GithubEventsSeenRepo,
) -> Result<AppliedOutcome, ApplyError> {
    if seen.is_seen(tx, github_event_id).await? {
        return Ok(AppliedOutcome::Duplicate);
    }

    let outcome = dispatch(tx, github_event_id, event).await;

    // Mark seen even when dispatch returned an error — the Tx carries
    // the warning row and we don't want to re-fire the same failing
    // event on retry. The caller commits to persist both.
    seen.mark_seen(tx, github_event_id, event.kind()).await?;

    outcome
}

async fn dispatch(
    tx: &mut Transaction<'_, Sqlite>,
    github_event_id: &str,
    event: &CanonicalGitHubEvent,
) -> Result<AppliedOutcome, ApplyError> {
    match event {
        CanonicalGitHubEvent::PrOpened {
            pr_number,
            head_branch,
        } => apply_pr_opened(tx, github_event_id, *pr_number, head_branch).await,

        // Non-state-changing events — recorded as seen, no outbox row.
        CanonicalGitHubEvent::PrUpdated { .. }
        | CanonicalGitHubEvent::PrComment { .. }
        | CanonicalGitHubEvent::PrClosed { .. } => Ok(AppliedOutcome::Ignored),

        CanonicalGitHubEvent::ReviewApproved {
            pr_number,
            reviewer,
        } => {
            apply_transition(
                tx,
                github_event_id,
                *pr_number,
                AssignmentPhase::Approved,
                TransitionActorRole::ReviewerHand,
                format!("github/reviewer/{reviewer}"),
                format!("PR #{pr_number} approved by {reviewer}"),
                None,
            )
            .await
        }

        CanonicalGitHubEvent::ReviewChangesRequested {
            pr_number,
            reviewer,
        } => {
            apply_transition(
                tx,
                github_event_id,
                *pr_number,
                AssignmentPhase::Rejected,
                TransitionActorRole::ReviewerHand,
                format!("github/reviewer/{reviewer}"),
                format!("PR #{pr_number} changes requested by {reviewer}"),
                None,
            )
            .await
        }

        CanonicalGitHubEvent::CheckRunStatus {
            pr_number,
            check_name,
            status,
        } => {
            if is_failure_conclusion(status) {
                // Acceptance: CI failure → Rejected, reason must link to
                // the failing run so the audit trail has a clickable hop
                // back to GitHub. The reason field is the outbox payload
                // attribute consumers use for that link.
                let actor = format!("github/check_run/{check_name}");
                let reason = format!("CI check {check_name} concluded {status} on PR #{pr_number}");
                let link = Some(json!({
                    "check_name": check_name,
                    "conclusion": status,
                }));
                apply_transition(
                    tx,
                    github_event_id,
                    *pr_number,
                    AssignmentPhase::Rejected,
                    TransitionActorRole::ReviewerHand,
                    actor,
                    reason,
                    link,
                )
                .await
            } else {
                Ok(AppliedOutcome::Ignored)
            }
        }

        CanonicalGitHubEvent::PrMerged {
            pr_number,
            merge_commit_sha,
        } => {
            // Acceptance: Epic PR merge advances the MergeHand assignment
            // to Merged. The validator enforces that the source phase is
            // ReadyForMerge and that the actor role is MergeHand; any
            // other assignment mirrored to the PR produces a warning.
            let actor = APPLIER_ACTOR.to_string();
            let reason = format!("PR #{pr_number} merged ({merge_commit_sha})");
            let link = Some(json!({
                "merge_commit_sha": merge_commit_sha,
            }));
            apply_transition(
                tx,
                github_event_id,
                *pr_number,
                AssignmentPhase::Merged,
                TransitionActorRole::MergeHand,
                actor,
                reason,
                link,
            )
            .await
        }
    }
}

/// PR-opened handler: resolve the Assignment by its Hand branch and
/// populate `github_artifact_*`. Idempotent — if the assignment already
/// has the same artifact recorded the call is an
/// [`AppliedOutcome::Ignored`]; if it has a different artifact the
/// incoming PR number wins (latest PR for the branch is the live one).
async fn apply_pr_opened(
    tx: &mut Transaction<'_, Sqlite>,
    github_event_id: &str,
    pr_number: i64,
    head_branch: &str,
) -> Result<AppliedOutcome, ApplyError> {
    let assignment = lookup_assignment_by_branch(tx, head_branch).await?;

    let Some(assignment) = assignment else {
        emit_orphan_warning(tx, github_event_id, pr_number, head_branch, "pr_opened").await?;
        return Ok(AppliedOutcome::NoAssignment { pr_number });
    };

    // Already recorded the same (branch, pr_number) — nothing to do.
    if assignment.github_artifact_pr_number == Some(pr_number)
        && assignment.github_artifact_branch.as_deref() == Some(head_branch)
    {
        return Ok(AppliedOutcome::Ignored);
    }

    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE assignments SET github_artifact_branch = ?, github_artifact_pr_number = ?, \
         updated_at = ? WHERE id = ?",
    )
    .bind(head_branch)
    .bind(pr_number)
    .bind(&now)
    .bind(assignment.id)
    .execute(&mut **tx)
    .await?;

    let payload = json!({
        "github_event_id": github_event_id,
        "pr_number": pr_number,
        "branch": head_branch,
    });
    emit_outbox_event(
        tx,
        EVT_ARTIFACT_RECORDED,
        &assignment.assignment_id,
        APPLIER_ACTOR,
        &payload,
    )
    .await?;

    Ok(AppliedOutcome::ArtifactRecorded {
        assignment_id: assignment.id,
        pr_number,
    })
}

#[allow(clippy::too_many_arguments)]
async fn apply_transition(
    tx: &mut Transaction<'_, Sqlite>,
    github_event_id: &str,
    pr_number: i64,
    target_phase: AssignmentPhase,
    actor_role: TransitionActorRole,
    actor_id: String,
    reason: String,
    link: Option<serde_json::Value>,
) -> Result<AppliedOutcome, ApplyError> {
    let Some(assignment) = lookup_assignment_by_pr(tx, pr_number).await? else {
        emit_orphan_warning(tx, github_event_id, pr_number, "", target_phase.as_str()).await?;
        return Ok(AppliedOutcome::NoAssignment { pr_number });
    };

    let current_phase = assignment
        .assignment_phase
        .as_deref()
        .and_then(AssignmentPhase::from_str)
        .ok_or_else(|| {
            // An assignment row whose phase string is missing or unknown
            // is a structural corruption — surface it through the
            // validator's IllegalTransition variant so the outbox warning
            // records the observed value.
            ApplyError::Transition(TransitionError::IllegalTransition {
                from: "unknown",
                to: target_phase.as_str(),
            })
        })?;

    // Route through the same validator that `transition.rs` uses. No
    // override — the mirror must obey the full role-ownership map.
    if let Err(err) = validate_transition(
        current_phase,
        target_phase,
        current_phase,
        actor_role,
        false,
    ) {
        emit_illegal_transition_warning(
            tx,
            github_event_id,
            &assignment,
            current_phase,
            target_phase,
            actor_role,
            &err,
            &reason,
            link.as_ref(),
        )
        .await?;
        return Err(ApplyError::Transition(err));
    }

    // Validator passed — mirror the write path of
    // `transition_assignment_phase_inner` so `assignments` + `events`
    // stay in sync with a single authoritative actor_id.
    let new_event_version = assignment.event_version + 1;
    let event_db_id = insert_phase_event(
        tx,
        &assignment.spark_id,
        &actor_id,
        current_phase,
        target_phase,
        &reason,
    )
    .await?;

    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE assignments SET assignment_phase = ?, event_version = ?, updated_at = ?, \
         phase_changed_at = ?, phase_changed_by = ?, phase_actor_role = ?, phase_event_id = ? \
         WHERE id = ?",
    )
    .bind(target_phase.as_str())
    .bind(new_event_version)
    .bind(&now)
    .bind(&now)
    .bind(&actor_id)
    .bind(actor_role.as_str())
    .bind(event_db_id)
    .bind(assignment.id)
    .execute(&mut **tx)
    .await?;

    let payload = json!({
        "github_event_id": github_event_id,
        "pr_number": pr_number,
        "from_phase": current_phase.as_str(),
        "to_phase": target_phase.as_str(),
        "actor_role": actor_role.as_str(),
        "reason": reason,
        "link": link,
    });
    emit_outbox_event(
        tx,
        EVT_PHASE_TRANSITIONED,
        &assignment.assignment_id,
        &actor_id,
        &payload,
    )
    .await?;

    Ok(AppliedOutcome::Transitioned {
        assignment_id: assignment.id,
        from: current_phase,
        to: target_phase,
    })
}

async fn lookup_assignment_by_pr(
    tx: &mut Transaction<'_, Sqlite>,
    pr_number: i64,
) -> Result<Option<Assignment>, sqlx::Error> {
    sqlx::query_as::<_, Assignment>(
        "SELECT * FROM assignments WHERE github_artifact_pr_number = ? LIMIT 1",
    )
    .bind(pr_number)
    .fetch_optional(&mut **tx)
    .await
}

async fn lookup_assignment_by_branch(
    tx: &mut Transaction<'_, Sqlite>,
    branch: &str,
) -> Result<Option<Assignment>, sqlx::Error> {
    // Prefer an existing mirror row (github_artifact_branch set);
    // fall back to source_branch for the first PrOpened on a Hand's
    // branch.
    if let Some(row) = sqlx::query_as::<_, Assignment>(
        "SELECT * FROM assignments WHERE github_artifact_branch = ? LIMIT 1",
    )
    .bind(branch)
    .fetch_optional(&mut **tx)
    .await?
    {
        return Ok(Some(row));
    }
    sqlx::query_as::<_, Assignment>(
        "SELECT * FROM assignments WHERE source_branch = ? \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(branch)
    .fetch_optional(&mut **tx)
    .await
}

async fn insert_phase_event(
    tx: &mut Transaction<'_, Sqlite>,
    spark_id: &str,
    actor_id: &str,
    from: AssignmentPhase,
    to: AssignmentPhase,
    reason: &str,
) -> Result<i64, sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO events \
         (spark_id, actor, field_name, old_value, new_value, reason, timestamp, \
          actor_type, change_nature, session_id) \
         VALUES (?, ?, 'assignment_phase', ?, ?, ?, ?, 'system', NULL, NULL) \
         RETURNING id",
    )
    .bind(spark_id)
    .bind(actor_id)
    .bind(from.as_str())
    .bind(to.as_str())
    .bind(reason)
    .bind(&now)
    .fetch_one(&mut **tx)
    .await
}

#[allow(clippy::too_many_arguments)]
async fn emit_illegal_transition_warning(
    tx: &mut Transaction<'_, Sqlite>,
    github_event_id: &str,
    assignment: &Assignment,
    current: AssignmentPhase,
    attempted: AssignmentPhase,
    actor_role: TransitionActorRole,
    err: &TransitionError,
    reason: &str,
    link: Option<&serde_json::Value>,
) -> Result<(), ApplyError> {
    let payload = json!({
        "github_event_id": github_event_id,
        "assignment_id": assignment.assignment_id,
        "current_phase": current.as_str(),
        "attempted_phase": attempted.as_str(),
        "attempted_role": actor_role.as_str(),
        "validator_error": err.to_string(),
        "reason": reason,
        "link": link,
    });
    emit_outbox_event(
        tx,
        EVT_ILLEGAL_TRANSITION_WARNING,
        &assignment.assignment_id,
        APPLIER_ACTOR,
        &payload,
    )
    .await
}

async fn emit_orphan_warning(
    tx: &mut Transaction<'_, Sqlite>,
    github_event_id: &str,
    pr_number: i64,
    branch: &str,
    attempted: &str,
) -> Result<(), ApplyError> {
    // Orphan events have no assignment_id to attach to. The outbox
    // schema requires one, so we stamp a synthetic marker so consumers
    // can filter `assignment_id='github-orphan'` to find unrouted events.
    let payload = json!({
        "github_event_id": github_event_id,
        "pr_number": pr_number,
        "branch": branch,
        "attempted": attempted,
    });
    emit_outbox_event(
        tx,
        EVT_ORPHAN_EVENT_WARNING,
        "github-orphan",
        APPLIER_ACTOR,
        &payload,
    )
    .await
}

async fn emit_outbox_event(
    tx: &mut Transaction<'_, Sqlite>,
    event_type: &str,
    assignment_id: &str,
    actor_id: &str,
    payload: &serde_json::Value,
) -> Result<(), ApplyError> {
    let payload_str =
        serde_json::to_string(payload).map_err(|e| ApplyError::Serialization(e.to_string()))?;
    let event_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO event_outbox \
         (event_id, schema_version, timestamp, assignment_id, actor_id, event_type, payload) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event_id)
    .bind(APPLIER_SCHEMA_VERSION)
    .bind(&now)
    .bind(assignment_id)
    .bind(actor_id)
    .bind(event_type)
    .bind(&payload_str)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// GitHub's `check_run.conclusion` values that represent CI failure for
/// the mirror's purposes. `action_required` is included because it blocks
/// the PR until resolved; anything else (`success`, `neutral`, `skipped`,
/// `stale`, in-progress) is treated as non-failing.
fn is_failure_conclusion(status: &str) -> bool {
    matches!(
        status,
        "failure" | "timed_out" | "cancelled" | "action_required" | "startup_failure"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_failure_conclusion_matches_failing_states() {
        for s in [
            "failure",
            "timed_out",
            "cancelled",
            "action_required",
            "startup_failure",
        ] {
            assert!(is_failure_conclusion(s), "{s} should count as failure");
        }
    }

    #[test]
    fn is_failure_conclusion_rejects_success_states() {
        for s in ["success", "neutral", "skipped", "stale", "in_progress", ""] {
            assert!(!is_failure_conclusion(s), "{s} should not count as failure");
        }
    }
}
