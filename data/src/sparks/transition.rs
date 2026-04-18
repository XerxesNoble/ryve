// SPDX-License-Identifier: AGPL-3.0-or-later

//! Assignment-phase transition validator with role-based ownership.
//!
//! This module is the **sole code path** that may mutate
//! `assignments.assignment_phase`. Direct SQL UPDATEs to that column
//! from any other module are forbidden.
//!
//! # Legal transition map
//!
//! ```text
//! Assigned        → InProgress
//! InProgress      → AwaitingReview | Stuck
//! AwaitingReview  → Approved | Rejected | Stuck
//! Rejected        → InRepair
//! InRepair        → AwaitingReview | Stuck
//! Approved        → ReadyForMerge
//! ReadyForMerge   → Merged
//! Stuck           → InProgress (Head/Director override only)
//! ```
//!
//! # Role ownership
//!
//! | Transition                    | Authorized roles              |
//! |-------------------------------|-------------------------------|
//! | Assigned → InProgress         | Hand                          |
//! | InProgress → AwaitingReview   | Hand                          |
//! | AwaitingReview → Approved     | ReviewerHand                  |
//! | AwaitingReview → Rejected     | ReviewerHand                  |
//! | Rejected → InRepair           | Hand                          |
//! | InRepair → AwaitingReview     | Hand                          |
//! | Approved → ReadyForMerge      | MergeHand (auto)              |
//! | ReadyForMerge → Merged        | MergeHand                     |
//! | InProgress → Stuck            | Hand, Head, Director          |
//! | AwaitingReview → Stuck        | Hand, Head, Director          |
//! | InRepair → Stuck              | Hand, Head, Director          |
//! | Stuck → InProgress            | Head, Director (override)     |
//!
//! Head and Director may override any transition with the explicit
//! `override_role_check` flag. The `Stuck → InProgress` recovery path is
//! the only way out of `Stuck` and is authorized exclusively to
//! Head/Director — there is no non-override entry for it in the map.
//!
//! # Reviewer identity invariant
//!
//! A `ReviewerHand` approving or rejecting an assignment MUST be a
//! different actor than the assignment's author (`assignments.actor_id`).
//! This is enforced in two places:
//!
//! 1. **Selection time** — callers picking a reviewer from a pool use
//!    [`ensure_reviewer_not_author`] to reject a candidate whose
//!    `actor_id` matches the author. This is the primary line of defence
//!    and keeps the constraint visible to the selection policy.
//! 2. **Transition time** — [`transition_assignment_phase`] re-checks
//!    the identity before writing `Approved`/`Rejected`. This guards
//!    against a ReviewerHand whose role was reassigned mid-flight or a
//!    stale/forged selection, per the invariant that a reviewer cannot
//!    approve their own work even if the actor role changes mid-flight.
//!
//! Head/Director overrides are unaffected because they do not use the
//! `ReviewerHand` role.

use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use super::error::TransitionError;
use super::event_repo;
use super::heartbeat_watchdog::{LIVENESS_TRANSITIONED_EVENT_TYPE, LivenessTransitionedPayload};
use super::projector::CURRENT_SCHEMA_VERSION;
use super::types::{
    Assignment, AssignmentLiveness, AssignmentPhase, NewEvent, TransitionActorRole,
};

/// Default maximum number of Rejected → InRepair cycles an assignment may
/// accumulate before the transition path escalates it to
/// [`AssignmentLiveness::Stuck`]. Parent epic `ryve-cf05fd85` names
/// `workshop.repair_cycle_limit` as the knob; this is the fallback when a
/// workshop has not overridden it.
pub const DEFAULT_REPAIR_CYCLE_LIMIT: i64 = 3;

/// `actor_id` stamped on every repair-cycle-driven Stuck outbox row. Stable
/// so downstream subscribers can distinguish this escalation path from the
/// watchdog (which uses [`super::heartbeat_watchdog::WATCHDOG_ACTOR`]) while
/// still consuming the shared
/// [`LIVENESS_TRANSITIONED_EVENT_TYPE`] event shape.
pub const REPAIR_CYCLE_ESCALATION_ACTOR: &str = "repair-cycle-escalation";

/// A transition rule: (from_phase, to_phase) → list of authorized roles.
struct TransitionRule {
    from: AssignmentPhase,
    to: AssignmentPhase,
    authorized_roles: &'static [TransitionActorRole],
}

/// The complete, exhaustive legal transition map.
const LEGAL_TRANSITIONS: &[TransitionRule] = &[
    TransitionRule {
        from: AssignmentPhase::Assigned,
        to: AssignmentPhase::InProgress,
        authorized_roles: &[TransitionActorRole::Hand],
    },
    TransitionRule {
        from: AssignmentPhase::InProgress,
        to: AssignmentPhase::AwaitingReview,
        authorized_roles: &[TransitionActorRole::Hand],
    },
    TransitionRule {
        from: AssignmentPhase::AwaitingReview,
        to: AssignmentPhase::Approved,
        authorized_roles: &[TransitionActorRole::ReviewerHand],
    },
    TransitionRule {
        from: AssignmentPhase::AwaitingReview,
        to: AssignmentPhase::Rejected,
        authorized_roles: &[TransitionActorRole::ReviewerHand],
    },
    TransitionRule {
        from: AssignmentPhase::Rejected,
        to: AssignmentPhase::InRepair,
        authorized_roles: &[TransitionActorRole::Hand],
    },
    TransitionRule {
        from: AssignmentPhase::InRepair,
        to: AssignmentPhase::AwaitingReview,
        authorized_roles: &[TransitionActorRole::Hand],
    },
    TransitionRule {
        from: AssignmentPhase::Approved,
        to: AssignmentPhase::ReadyForMerge,
        authorized_roles: &[TransitionActorRole::MergeHand],
    },
    TransitionRule {
        from: AssignmentPhase::ReadyForMerge,
        to: AssignmentPhase::Merged,
        authorized_roles: &[TransitionActorRole::MergeHand],
    },
    // Stuck entry paths: a Hand reports itself stuck from any active
    // working phase. Head/Director can also drive these transitions via
    // the override flag, but they are already covered by `can_override`
    // and do not need their own entries.
    TransitionRule {
        from: AssignmentPhase::InProgress,
        to: AssignmentPhase::Stuck,
        authorized_roles: &[TransitionActorRole::Hand],
    },
    TransitionRule {
        from: AssignmentPhase::AwaitingReview,
        to: AssignmentPhase::Stuck,
        authorized_roles: &[TransitionActorRole::Hand],
    },
    TransitionRule {
        from: AssignmentPhase::InRepair,
        to: AssignmentPhase::Stuck,
        authorized_roles: &[TransitionActorRole::Hand],
    },
    // Stuck recovery: Head/Director only, and only via the explicit
    // override path. The empty `authorized_roles` list means no
    // non-override role is allowed to take this transition.
    TransitionRule {
        from: AssignmentPhase::Stuck,
        to: AssignmentPhase::InProgress,
        authorized_roles: &[],
    },
];

/// Look up the transition rule for a (from, to) pair.
fn find_rule(from: AssignmentPhase, to: AssignmentPhase) -> Option<&'static TransitionRule> {
    LEGAL_TRANSITIONS
        .iter()
        .find(|r| r.from == from && r.to == to)
}

/// Validate that the transition from `current` to `target` is legal,
/// that `expected_previous` matches `current` (replay safety), and that
/// the actor's role is authorized.
///
/// Returns `Ok(())` on success, or a specific `TransitionError`.
pub fn validate_transition(
    current_phase: AssignmentPhase,
    target_phase: AssignmentPhase,
    expected_previous_phase: AssignmentPhase,
    actor_role: TransitionActorRole,
    override_role_check: bool,
) -> Result<(), TransitionError> {
    // 1. Out-of-order replay safety: expected must match actual.
    if current_phase != expected_previous_phase {
        return Err(TransitionError::PhaseMismatch {
            expected: expected_previous_phase.as_str(),
            actual: current_phase.as_str(),
        });
    }

    // 2. Transition must be in the legal map.
    let rule =
        find_rule(current_phase, target_phase).ok_or(TransitionError::IllegalTransition {
            from: current_phase.as_str(),
            to: target_phase.as_str(),
        })?;

    // 3. Role authorization (Head/Director may override).
    if (!override_role_check || !actor_role.can_override())
        && !rule.authorized_roles.contains(&actor_role)
    {
        let authorized = rule
            .authorized_roles
            .iter()
            .map(|r| r.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(TransitionError::Unauthorized {
            role: actor_role.as_str(),
            from: current_phase.as_str(),
            to: target_phase.as_str(),
            authorized,
        });
    }

    Ok(())
}

/// Enforce the reviewer-identity invariant: a `ReviewerHand` driving an
/// `Approved` or `Rejected` transition cannot be the author of the
/// assignment.
///
/// Returns `Ok(())` for every non-reviewer role and for every target
/// phase other than `Approved`/`Rejected` — this function is safe to
/// call unconditionally from any transition path. Also used at selection
/// time via [`ensure_reviewer_not_author`].
pub fn validate_reviewer_not_author(
    actor_role: TransitionActorRole,
    target_phase: AssignmentPhase,
    reviewer_actor_id: &str,
    author_actor_id: &str,
) -> Result<(), TransitionError> {
    if actor_role != TransitionActorRole::ReviewerHand {
        return Ok(());
    }
    if !matches!(
        target_phase,
        AssignmentPhase::Approved | AssignmentPhase::Rejected
    ) {
        return Ok(());
    }
    ensure_reviewer_not_author(reviewer_actor_id, author_actor_id)
}

/// Selection-time check: reject a reviewer candidate whose `actor_id`
/// matches the assignment author's. Returns the same
/// [`TransitionError::ReviewerIsAuthor`] the transition-time check would
/// produce, so both paths surface identical error semantics.
pub fn ensure_reviewer_not_author(
    reviewer_actor_id: &str,
    author_actor_id: &str,
) -> Result<(), TransitionError> {
    if reviewer_actor_id == author_actor_id {
        return Err(TransitionError::ReviewerIsAuthor {
            reviewer_actor_id: reviewer_actor_id.to_string(),
        });
    }
    Ok(())
}

pub async fn transition_assignment_phase(
    pool: &SqlitePool,
    assignment_id: i64,
    actor_id: &str,
    actor_role: TransitionActorRole,
    target_phase: AssignmentPhase,
    expected_previous_phase: AssignmentPhase,
    event_version: i64,
) -> Result<Assignment, TransitionError> {
    transition_assignment_phase_inner(
        pool,
        TransitionPhaseRequest {
            assignment_id,
            actor_id,
            actor_role,
            target_phase,
            expected_previous_phase,
            override_role_check: false,
            event_version,
            repair_cycle_limit: DEFAULT_REPAIR_CYCLE_LIMIT,
        },
    )
    .await
}

pub async fn transition_assignment_phase_override(
    pool: &SqlitePool,
    assignment_id: i64,
    actor_id: &str,
    actor_role: TransitionActorRole,
    target_phase: AssignmentPhase,
    expected_previous_phase: AssignmentPhase,
    event_version: i64,
) -> Result<Assignment, TransitionError> {
    transition_assignment_phase_inner(
        pool,
        TransitionPhaseRequest {
            assignment_id,
            actor_id,
            actor_role,
            target_phase,
            expected_previous_phase,
            override_role_check: true,
            event_version,
            repair_cycle_limit: DEFAULT_REPAIR_CYCLE_LIMIT,
        },
    )
    .await
}

/// Explicit-limit variant of [`transition_assignment_phase`]. Workshops
/// thread their configured limit through [`PhaseTransitionArgs`] so the
/// repair-cycle → Stuck escalation honours `workshop.repair_cycle_limit`
/// rather than falling back to [`DEFAULT_REPAIR_CYCLE_LIMIT`]. The struct
/// input keeps the API readable as the surface grows beyond the seven
/// positional arguments [`transition_assignment_phase`] already carries.
pub async fn transition_assignment_phase_with_limit(
    pool: &SqlitePool,
    args: PhaseTransitionArgs<'_>,
) -> Result<Assignment, TransitionError> {
    transition_assignment_phase_inner(
        pool,
        TransitionPhaseRequest {
            assignment_id: args.assignment_id,
            actor_id: args.actor_id,
            actor_role: args.actor_role,
            target_phase: args.target_phase,
            expected_previous_phase: args.expected_previous_phase,
            override_role_check: false,
            event_version: args.event_version,
            repair_cycle_limit: args.repair_cycle_limit,
        },
    )
    .await
}

/// Input for [`transition_assignment_phase_with_limit`]. Mirrors the
/// positional arguments of [`transition_assignment_phase`] plus an
/// explicit `repair_cycle_limit`.
pub struct PhaseTransitionArgs<'a> {
    pub assignment_id: i64,
    pub actor_id: &'a str,
    pub actor_role: TransitionActorRole,
    pub target_phase: AssignmentPhase,
    pub expected_previous_phase: AssignmentPhase,
    pub event_version: i64,
    pub repair_cycle_limit: i64,
}

struct TransitionPhaseRequest<'a> {
    assignment_id: i64,
    actor_id: &'a str,
    actor_role: TransitionActorRole,
    target_phase: AssignmentPhase,
    expected_previous_phase: AssignmentPhase,
    override_role_check: bool,
    event_version: i64,
    repair_cycle_limit: i64,
}

/// Execute a phase transition: validate, UPDATE state + phase-tracking columns,
/// INSERT an event into the outbox — all in a single transaction.
async fn transition_assignment_phase_inner(
    pool: &SqlitePool,
    request: TransitionPhaseRequest<'_>,
) -> Result<Assignment, TransitionError> {
    let mut tx = pool.begin().await?;

    let TransitionPhaseRequest {
        assignment_id,
        actor_id,
        actor_role,
        target_phase,
        expected_previous_phase,
        override_role_check,
        event_version,
        repair_cycle_limit,
    } = request;

    let row = sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE id = ?")
        .bind(assignment_id)
        .fetch_optional(&mut *tx)
        .await?;

    let assignment = row.ok_or_else(|| TransitionError::AssignmentNotFound {
        assignment_id: assignment_id.to_string(),
    })?;

    let current_phase_str = assignment.assignment_phase.as_deref().unwrap_or("unknown");
    let current_phase =
        AssignmentPhase::from_str(current_phase_str).ok_or(TransitionError::IllegalTransition {
            from: "unknown",
            to: target_phase.as_str(),
        })?;

    validate_transition(
        current_phase,
        target_phase,
        expected_previous_phase,
        actor_role,
        override_role_check,
    )?;

    // Reviewer identity: enforced independently of override_role_check
    // because the rule ("a reviewer cannot approve their own work")
    // follows the effective role, not the override path. Head/Director
    // overrides skip this because they don't transition as ReviewerHand.
    validate_reviewer_not_author(actor_role, target_phase, actor_id, &assignment.actor_id)?;

    let event_id = event_repo::record_in_tx(
        &mut tx,
        NewEvent {
            spark_id: assignment.spark_id.clone(),
            actor: actor_id.to_string(),
            field_name: "assignment_phase".into(),
            old_value: Some(current_phase.as_str().to_string()),
            new_value: Some(target_phase.as_str().to_string()),
            reason: None,
            actor_type: None,
            change_nature: None,
            session_id: None,
        },
    )
    .await
    .map_err(|e| {
        TransitionError::Database(match e {
            super::error::SparksError::Database(db_err) => db_err,
            other => sqlx::Error::Protocol(other.to_string()),
        })
    })?;

    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE assignments SET assignment_phase = ?, event_version = ?, updated_at = ?, \
         phase_changed_at = ?, phase_changed_by = ?, phase_actor_role = ?, phase_event_id = ? \
         WHERE id = ?",
    )
    .bind(target_phase.as_str())
    .bind(event_version)
    .bind(&now)
    .bind(&now)
    .bind(actor_id)
    .bind(actor_role.as_str())
    .bind(event_id)
    .bind(assignment_id)
    .execute(&mut *tx)
    .await?;

    // Repair-cycle bookkeeping: every Rejected → InRepair edge counts as one
    // re-entry into the repair loop. Past the configured limit the
    // assignment is escalated to `AssignmentLiveness::Stuck` inside the same
    // transaction, emitting the canonical LivenessTransitioned outbox row
    // the watchdog writes on a heartbeat timeout. The Hand that drove the
    // phase transition is the proximate cause but the escalation itself is
    // system-originated — the outbox row carries
    // [`REPAIR_CYCLE_ESCALATION_ACTOR`], not the Hand's actor_id, preserving
    // the "Hand cannot self-transition to Stuck" invariant.
    if current_phase == AssignmentPhase::Rejected && target_phase == AssignmentPhase::InRepair {
        let new_count = sqlx::query_scalar::<_, i64>(
            "UPDATE assignments SET repair_cycle_count = repair_cycle_count + 1 \
             WHERE id = ? RETURNING repair_cycle_count",
        )
        .bind(assignment_id)
        .fetch_one(&mut *tx)
        .await?;

        if new_count > repair_cycle_limit {
            escalate_to_stuck_in_tx(
                &mut tx,
                assignment_id,
                &assignment.spark_id,
                REPAIR_CYCLE_ESCALATION_ACTOR,
                &now,
                new_count,
            )
            .await?;
        }
    }

    let updated = sqlx::query_as::<_, Assignment>("SELECT * FROM assignments WHERE id = ?")
        .bind(assignment_id)
        .fetch_one(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(updated)
}

/// Force an active assignment's liveness to [`AssignmentLiveness::Stuck`]
/// and emit the canonical outbox event. Used by the repair-cycle
/// escalation path inside [`transition_assignment_phase_inner`] and by the
/// Head/Director override path in [`transition_liveness_to_stuck`].
///
/// The UPDATE is conditional on `status='active'` and on the current
/// liveness not already being `Stuck`, so a concurrent watchdog tick that
/// beat us does not produce a duplicate event. When the UPDATE matches zero
/// rows (already Stuck, or no longer active) the function is a no-op.
async fn escalate_to_stuck_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    assignment_id: i64,
    spark_id: &str,
    actor_id: &str,
    observed_at: &str,
    repair_cycle_count: i64,
) -> Result<(), TransitionError> {
    // Capture the pre-update liveness so the emitted event's
    // `from_liveness` reflects reality. The assignment may already be
    // `AtRisk` when the repair-cycle counter escalates (e.g. heartbeat
    // has aged between the last check and this call); pinning
    // `from_liveness = Healthy` would mislead downstream auditors that
    // replay the outbox.
    let pre_row = sqlx::query_as::<_, (String,)>(
        "SELECT liveness FROM assignments \
         WHERE id = ? AND status = 'active' AND liveness != ?",
    )
    .bind(assignment_id)
    .bind(AssignmentLiveness::Stuck.as_str())
    .fetch_optional(&mut **tx)
    .await?;

    let Some((prev_liveness_str,)) = pre_row else {
        return Ok(());
    };
    let from_liveness =
        AssignmentLiveness::from_str(&prev_liveness_str).unwrap_or(AssignmentLiveness::Healthy);

    let row = sqlx::query_as::<_, (String, String, Option<String>)>(
        "UPDATE assignments SET liveness = ? \
         WHERE id = ? AND status = 'active' AND liveness != ? \
         RETURNING assignment_id, liveness, last_heartbeat_at",
    )
    .bind(AssignmentLiveness::Stuck.as_str())
    .bind(assignment_id)
    .bind(AssignmentLiveness::Stuck.as_str())
    .fetch_optional(&mut **tx)
    .await?;

    let Some((assignment_id_str, _, last_heartbeat_at)) = row else {
        return Ok(());
    };

    let payload = LivenessTransitionedPayload {
        assignment_id: assignment_id_str.clone(),
        spark_id: spark_id.to_string(),
        from_liveness,
        to_liveness: AssignmentLiveness::Stuck,
        observed_at: observed_at.to_string(),
        last_heartbeat_at,
        age_secs: repair_cycle_count,
    };
    let payload_json = serde_json::to_string(&payload).map_err(|e| {
        TransitionError::Database(sqlx::Error::Protocol(format!(
            "serialize liveness payload: {e}"
        )))
    })?;

    let event_id = format!("evt-{}", Uuid::new_v4());
    sqlx::query(
        "INSERT INTO event_outbox \
         (event_id, schema_version, timestamp, assignment_id, actor_id, event_type, payload) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event_id)
    .bind(CURRENT_SCHEMA_VERSION as i64)
    .bind(observed_at)
    .bind(&assignment_id_str)
    .bind(actor_id)
    .bind(LIVENESS_TRANSITIONED_EVENT_TYPE)
    .bind(&payload_json)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Enforce the "Hand cannot self-transition to Stuck" invariant. The Stuck
/// liveness value is only emittable by the watchdog (via its own path) or
/// by a Head/Director override through
/// [`transition_liveness_to_stuck`]. Hand, ReviewerHand, and MergeHand are
/// all rejected — none of the execution roles may flip their own
/// assignment to Stuck.
pub fn validate_stuck_origin(actor_role: TransitionActorRole) -> Result<(), TransitionError> {
    if actor_role.can_override() {
        return Ok(());
    }
    Err(TransitionError::Unauthorized {
        role: actor_role.as_str(),
        from: "any",
        to: AssignmentLiveness::Stuck.as_str(),
        authorized: "head, director".to_string(),
    })
}

/// Head/Director-only override that forces an active assignment's
/// liveness to [`AssignmentLiveness::Stuck`] and emits the canonical
/// outbox event. Any other role (including [`TransitionActorRole::Hand`])
/// is rejected with [`TransitionError::Unauthorized`]; see
/// [`validate_stuck_origin`].
///
/// The emitted outbox row is stamped with the caller's `actor_id` so
/// subscribers can distinguish an override from the repair-cycle
/// escalation ([`REPAIR_CYCLE_ESCALATION_ACTOR`]) and the watchdog
/// ([`super::heartbeat_watchdog::WATCHDOG_ACTOR`]).
pub async fn transition_liveness_to_stuck(
    pool: &SqlitePool,
    assignment_id: i64,
    actor_id: &str,
    actor_role: TransitionActorRole,
) -> Result<(), TransitionError> {
    validate_stuck_origin(actor_role)?;

    let mut tx = pool.begin().await?;

    let row = sqlx::query_as::<_, (String,)>("SELECT spark_id FROM assignments WHERE id = ?")
        .bind(assignment_id)
        .fetch_optional(&mut *tx)
        .await?;
    let (spark_id,) = row.ok_or_else(|| TransitionError::AssignmentNotFound {
        assignment_id: assignment_id.to_string(),
    })?;

    let now = Utc::now().to_rfc3339();
    escalate_to_stuck_in_tx(&mut tx, assignment_id, &spark_id, actor_id, &now, 0).await?;

    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pure validation tests (no database) ─────────────

    #[test]
    fn legal_transition_assigned_to_in_progress() {
        assert!(
            validate_transition(
                AssignmentPhase::Assigned,
                AssignmentPhase::InProgress,
                AssignmentPhase::Assigned,
                TransitionActorRole::Hand,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn legal_transition_in_progress_to_awaiting_review() {
        assert!(
            validate_transition(
                AssignmentPhase::InProgress,
                AssignmentPhase::AwaitingReview,
                AssignmentPhase::InProgress,
                TransitionActorRole::Hand,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn legal_transition_awaiting_review_to_approved() {
        assert!(
            validate_transition(
                AssignmentPhase::AwaitingReview,
                AssignmentPhase::Approved,
                AssignmentPhase::AwaitingReview,
                TransitionActorRole::ReviewerHand,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn legal_transition_awaiting_review_to_rejected() {
        assert!(
            validate_transition(
                AssignmentPhase::AwaitingReview,
                AssignmentPhase::Rejected,
                AssignmentPhase::AwaitingReview,
                TransitionActorRole::ReviewerHand,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn legal_transition_rejected_to_in_repair() {
        assert!(
            validate_transition(
                AssignmentPhase::Rejected,
                AssignmentPhase::InRepair,
                AssignmentPhase::Rejected,
                TransitionActorRole::Hand,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn legal_transition_in_repair_to_awaiting_review() {
        assert!(
            validate_transition(
                AssignmentPhase::InRepair,
                AssignmentPhase::AwaitingReview,
                AssignmentPhase::InRepair,
                TransitionActorRole::Hand,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn legal_transition_approved_to_ready_for_merge() {
        assert!(
            validate_transition(
                AssignmentPhase::Approved,
                AssignmentPhase::ReadyForMerge,
                AssignmentPhase::Approved,
                TransitionActorRole::MergeHand,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn legal_transition_ready_for_merge_to_merged() {
        assert!(
            validate_transition(
                AssignmentPhase::ReadyForMerge,
                AssignmentPhase::Merged,
                AssignmentPhase::ReadyForMerge,
                TransitionActorRole::MergeHand,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn illegal_transition_is_rejected() {
        // Assigned → Merged is not legal.
        let err = validate_transition(
            AssignmentPhase::Assigned,
            AssignmentPhase::Merged,
            AssignmentPhase::Assigned,
            TransitionActorRole::Hand,
            false,
        )
        .unwrap_err();
        assert!(
            matches!(err, TransitionError::IllegalTransition { .. }),
            "expected IllegalTransition, got {err:?}"
        );
    }

    #[test]
    fn illegal_transition_backward_step() {
        // InProgress → Assigned is not in the map.
        let err = validate_transition(
            AssignmentPhase::InProgress,
            AssignmentPhase::Assigned,
            AssignmentPhase::InProgress,
            TransitionActorRole::Hand,
            false,
        )
        .unwrap_err();
        assert!(matches!(err, TransitionError::IllegalTransition { .. }));
    }

    #[test]
    fn phase_mismatch_rejects_transition() {
        // Caller expects Assigned, but actual is InProgress.
        let err = validate_transition(
            AssignmentPhase::InProgress,
            AssignmentPhase::AwaitingReview,
            AssignmentPhase::Assigned, // wrong expectation
            TransitionActorRole::Hand,
            false,
        )
        .unwrap_err();
        assert!(
            matches!(err, TransitionError::PhaseMismatch { .. }),
            "expected PhaseMismatch, got {err:?}"
        );
    }

    #[test]
    fn unauthorized_role_is_rejected() {
        // Hand tries to approve — only ReviewerHand may do that.
        let err = validate_transition(
            AssignmentPhase::AwaitingReview,
            AssignmentPhase::Approved,
            AssignmentPhase::AwaitingReview,
            TransitionActorRole::Hand,
            false,
        )
        .unwrap_err();
        assert!(
            matches!(err, TransitionError::Unauthorized { .. }),
            "expected Unauthorized, got {err:?}"
        );
    }

    #[test]
    fn reviewer_cannot_start_work() {
        // ReviewerHand tries Assigned → InProgress (Hand-only).
        let err = validate_transition(
            AssignmentPhase::Assigned,
            AssignmentPhase::InProgress,
            AssignmentPhase::Assigned,
            TransitionActorRole::ReviewerHand,
            false,
        )
        .unwrap_err();
        assert!(matches!(err, TransitionError::Unauthorized { .. }));
    }

    #[test]
    fn merge_hand_cannot_approve() {
        let err = validate_transition(
            AssignmentPhase::AwaitingReview,
            AssignmentPhase::Approved,
            AssignmentPhase::AwaitingReview,
            TransitionActorRole::MergeHand,
            false,
        )
        .unwrap_err();
        assert!(matches!(err, TransitionError::Unauthorized { .. }));
    }

    #[test]
    fn head_override_bypasses_role_check() {
        // Head overrides the Hand-only Assigned → InProgress transition.
        assert!(
            validate_transition(
                AssignmentPhase::Assigned,
                AssignmentPhase::InProgress,
                AssignmentPhase::Assigned,
                TransitionActorRole::Head,
                true, // override
            )
            .is_ok()
        );
    }

    #[test]
    fn director_override_bypasses_role_check() {
        // Director overrides ReviewerHand-only AwaitingReview → Approved.
        assert!(
            validate_transition(
                AssignmentPhase::AwaitingReview,
                AssignmentPhase::Approved,
                AssignmentPhase::AwaitingReview,
                TransitionActorRole::Director,
                true, // override
            )
            .is_ok()
        );
    }

    #[test]
    fn override_without_override_role_is_rejected() {
        // A Hand passes override=true but Hand.can_override() is false.
        let err = validate_transition(
            AssignmentPhase::AwaitingReview,
            AssignmentPhase::Approved,
            AssignmentPhase::AwaitingReview,
            TransitionActorRole::Hand,
            true, // override flag, but Hand can't override
        )
        .unwrap_err();
        assert!(matches!(err, TransitionError::Unauthorized { .. }));
    }

    #[test]
    fn override_does_not_bypass_illegal_transition() {
        // Even a Director cannot perform an illegal transition.
        let err = validate_transition(
            AssignmentPhase::Assigned,
            AssignmentPhase::Merged,
            AssignmentPhase::Assigned,
            TransitionActorRole::Director,
            true,
        )
        .unwrap_err();
        assert!(matches!(err, TransitionError::IllegalTransition { .. }));
    }

    #[test]
    fn override_does_not_bypass_phase_mismatch() {
        // Even a Director gets rejected on phase mismatch.
        let err = validate_transition(
            AssignmentPhase::InProgress,
            AssignmentPhase::AwaitingReview,
            AssignmentPhase::Assigned, // wrong
            TransitionActorRole::Director,
            true,
        )
        .unwrap_err();
        assert!(matches!(err, TransitionError::PhaseMismatch { .. }));
    }

    #[test]
    fn full_happy_path_walk() {
        // Walk the entire state machine from Assigned to Merged.
        let steps: &[(AssignmentPhase, AssignmentPhase, TransitionActorRole)] = &[
            (
                AssignmentPhase::Assigned,
                AssignmentPhase::InProgress,
                TransitionActorRole::Hand,
            ),
            (
                AssignmentPhase::InProgress,
                AssignmentPhase::AwaitingReview,
                TransitionActorRole::Hand,
            ),
            (
                AssignmentPhase::AwaitingReview,
                AssignmentPhase::Approved,
                TransitionActorRole::ReviewerHand,
            ),
            (
                AssignmentPhase::Approved,
                AssignmentPhase::ReadyForMerge,
                TransitionActorRole::MergeHand,
            ),
            (
                AssignmentPhase::ReadyForMerge,
                AssignmentPhase::Merged,
                TransitionActorRole::MergeHand,
            ),
        ];

        for (from, to, role) in steps {
            validate_transition(*from, *to, *from, *role, false).unwrap_or_else(|e| {
                panic!("expected {from:?} → {to:?} by {role:?} to succeed, got {e}")
            });
        }
    }

    #[test]
    fn rejection_repair_resubmit_path() {
        // AwaitingReview → Rejected → InRepair → AwaitingReview → Approved
        let steps: &[(AssignmentPhase, AssignmentPhase, TransitionActorRole)] = &[
            (
                AssignmentPhase::AwaitingReview,
                AssignmentPhase::Rejected,
                TransitionActorRole::ReviewerHand,
            ),
            (
                AssignmentPhase::Rejected,
                AssignmentPhase::InRepair,
                TransitionActorRole::Hand,
            ),
            (
                AssignmentPhase::InRepair,
                AssignmentPhase::AwaitingReview,
                TransitionActorRole::Hand,
            ),
            (
                AssignmentPhase::AwaitingReview,
                AssignmentPhase::Approved,
                TransitionActorRole::ReviewerHand,
            ),
        ];

        for (from, to, role) in steps {
            validate_transition(*from, *to, *from, *role, false).unwrap_or_else(|e| {
                panic!("expected {from:?} → {to:?} by {role:?} to succeed, got {e}")
            });
        }
    }

    // ── Reviewer identity invariant ─────────────────────

    #[test]
    fn ensure_reviewer_not_author_rejects_same_actor() {
        let err =
            ensure_reviewer_not_author("actor-42", "actor-42").expect_err("must reject identity");
        match err {
            TransitionError::ReviewerIsAuthor { reviewer_actor_id } => {
                assert_eq!(reviewer_actor_id, "actor-42");
            }
            other => panic!("expected ReviewerIsAuthor, got {other:?}"),
        }
    }

    #[test]
    fn ensure_reviewer_not_author_accepts_distinct_actors() {
        assert!(ensure_reviewer_not_author("actor-reviewer", "actor-author").is_ok());
    }

    #[test]
    fn validate_reviewer_not_author_blocks_self_approval() {
        let err = validate_reviewer_not_author(
            TransitionActorRole::ReviewerHand,
            AssignmentPhase::Approved,
            "actor-shared",
            "actor-shared",
        )
        .expect_err("self-approval must be rejected");
        assert!(matches!(err, TransitionError::ReviewerIsAuthor { .. }));
    }

    #[test]
    fn validate_reviewer_not_author_blocks_self_rejection() {
        let err = validate_reviewer_not_author(
            TransitionActorRole::ReviewerHand,
            AssignmentPhase::Rejected,
            "actor-shared",
            "actor-shared",
        )
        .expect_err("self-rejection must be rejected");
        assert!(matches!(err, TransitionError::ReviewerIsAuthor { .. }));
    }

    #[test]
    fn validate_reviewer_not_author_allows_different_reviewer() {
        assert!(
            validate_reviewer_not_author(
                TransitionActorRole::ReviewerHand,
                AssignmentPhase::Approved,
                "actor-reviewer",
                "actor-author",
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_reviewer_not_author_is_noop_for_non_reviewer_roles() {
        // A Hand advancing InProgress → AwaitingReview must not trip the
        // reviewer-identity check even when author == actor (which is
        // the usual case for an author driving their own work forward).
        assert!(
            validate_reviewer_not_author(
                TransitionActorRole::Hand,
                AssignmentPhase::AwaitingReview,
                "actor-author",
                "actor-author",
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_reviewer_not_author_is_noop_for_non_review_targets() {
        // ReviewerHand role on a non-Approved/Rejected target phase is
        // illegal in `validate_transition`, but the identity check
        // itself should ignore it — the guard is belt-and-suspenders
        // and only fires on the approvals it protects.
        assert!(
            validate_reviewer_not_author(
                TransitionActorRole::ReviewerHand,
                AssignmentPhase::Merged,
                "actor-shared",
                "actor-shared",
            )
            .is_ok()
        );
    }

    #[test]
    fn all_legal_transitions_are_covered() {
        // 8 core happy-path edges + 3 Stuck entry edges + 1 Stuck
        // recovery (Head/Director override only) = 12 total.
        assert_eq!(
            LEGAL_TRANSITIONS.len(),
            12,
            "legal transition map should have exactly 12 entries"
        );
    }

    // ── Stuck-origin validation ─────────────────────────

    #[test]
    fn validate_stuck_origin_rejects_hand() {
        let err = validate_stuck_origin(TransitionActorRole::Hand).unwrap_err();
        match err {
            TransitionError::Unauthorized { role, to, .. } => {
                assert_eq!(role, "hand");
                assert_eq!(to, "stuck");
            }
            other => panic!("expected Unauthorized, got {other:?}"),
        }
    }

    #[test]
    fn validate_stuck_origin_rejects_reviewer_hand() {
        assert!(matches!(
            validate_stuck_origin(TransitionActorRole::ReviewerHand).unwrap_err(),
            TransitionError::Unauthorized { .. }
        ));
    }

    #[test]
    fn validate_stuck_origin_rejects_merge_hand() {
        assert!(matches!(
            validate_stuck_origin(TransitionActorRole::MergeHand).unwrap_err(),
            TransitionError::Unauthorized { .. }
        ));
    }

    #[test]
    fn validate_stuck_origin_accepts_head_and_director() {
        assert!(validate_stuck_origin(TransitionActorRole::Head).is_ok());
        assert!(validate_stuck_origin(TransitionActorRole::Director).is_ok());
    }

    // ── Stuck transitions ───────────────────────────────

    #[test]
    fn hand_can_report_in_progress_as_stuck() {
        validate_transition(
            AssignmentPhase::InProgress,
            AssignmentPhase::Stuck,
            AssignmentPhase::InProgress,
            TransitionActorRole::Hand,
            false,
        )
        .unwrap();
    }

    #[test]
    fn hand_can_report_awaiting_review_as_stuck() {
        validate_transition(
            AssignmentPhase::AwaitingReview,
            AssignmentPhase::Stuck,
            AssignmentPhase::AwaitingReview,
            TransitionActorRole::Hand,
            false,
        )
        .unwrap();
    }

    #[test]
    fn hand_can_report_in_repair_as_stuck() {
        validate_transition(
            AssignmentPhase::InRepair,
            AssignmentPhase::Stuck,
            AssignmentPhase::InRepair,
            TransitionActorRole::Hand,
            false,
        )
        .unwrap();
    }

    #[test]
    fn stuck_to_in_progress_requires_head_or_director_override() {
        // Hand cannot recover a stuck assignment, with or without the override flag.
        let err = validate_transition(
            AssignmentPhase::Stuck,
            AssignmentPhase::InProgress,
            AssignmentPhase::Stuck,
            TransitionActorRole::Hand,
            false,
        )
        .unwrap_err();
        assert!(
            matches!(err, TransitionError::Unauthorized { .. }),
            "Hand without override must be Unauthorized, got {err:?}"
        );

        let err = validate_transition(
            AssignmentPhase::Stuck,
            AssignmentPhase::InProgress,
            AssignmentPhase::Stuck,
            TransitionActorRole::Hand,
            true,
        )
        .unwrap_err();
        assert!(
            matches!(err, TransitionError::Unauthorized { .. }),
            "Hand with override flag still cannot can_override(), got {err:?}"
        );

        // ReviewerHand and MergeHand also cannot recover even with the
        // override flag — `can_override()` is gated on Head/Director.
        let err = validate_transition(
            AssignmentPhase::Stuck,
            AssignmentPhase::InProgress,
            AssignmentPhase::Stuck,
            TransitionActorRole::ReviewerHand,
            true,
        )
        .unwrap_err();
        assert!(matches!(err, TransitionError::Unauthorized { .. }));

        // Head and Director may override.
        validate_transition(
            AssignmentPhase::Stuck,
            AssignmentPhase::InProgress,
            AssignmentPhase::Stuck,
            TransitionActorRole::Head,
            true,
        )
        .unwrap();
        validate_transition(
            AssignmentPhase::Stuck,
            AssignmentPhase::InProgress,
            AssignmentPhase::Stuck,
            TransitionActorRole::Director,
            true,
        )
        .unwrap();
    }

    #[test]
    fn stuck_has_no_non_override_authorised_role() {
        // Explicitly: the Stuck → InProgress rule carries an empty
        // authorized_roles list. Without override, no role may drive it.
        for role in &[
            TransitionActorRole::Hand,
            TransitionActorRole::ReviewerHand,
            TransitionActorRole::MergeHand,
            TransitionActorRole::Head,
            TransitionActorRole::Director,
        ] {
            let err = validate_transition(
                AssignmentPhase::Stuck,
                AssignmentPhase::InProgress,
                AssignmentPhase::Stuck,
                *role,
                false,
            )
            .unwrap_err();
            assert!(
                matches!(err, TransitionError::Unauthorized { .. }),
                "{role:?} without override must be Unauthorized, got {err:?}"
            );
        }
    }
}
