// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Assignment-phase transition validator with role-based ownership.
//!
//! This module is the **sole code path** that may mutate
//! `hand_assignments.assignment_phase`. Direct SQL UPDATEs to that column
//! from any other module are forbidden.
//!
//! # Legal transition map
//!
//! ```text
//! Assigned        → InProgress
//! InProgress      → AwaitingReview
//! AwaitingReview  → Approved | Rejected
//! Rejected        → InRepair
//! InRepair        → AwaitingReview
//! Approved        → ReadyForMerge
//! ReadyForMerge   → Merged
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
//!
//! Head and Director may override any transition with the explicit
//! `override_role_check` flag.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::TransitionError;
use super::types::{AssignmentPhase, HandAssignment, TransitionActorRole};

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

/// Transition an assignment's phase in the database. This is the **only**
/// function that may write to `hand_assignments.assignment_phase`.
///
/// # Arguments
///
/// * `pool` — database connection pool
/// * `assignment_id` — PK of the `hand_assignments` row
/// * `actor_id` — identifier of the actor performing the transition
///   (typically a session_id)
/// * `actor_role` — the role under which the actor is operating
/// * `target_phase` — the desired new phase
/// * `expected_previous_phase` — the phase the caller believes the
///   assignment is currently in; if it doesn't match, the transition is
///   rejected (out-of-order replay safety)
/// * `event_id` — the event that triggered this transition (for audit)
///
/// # Returns
///
/// The updated `HandAssignment` on success, or a `TransitionError`.
pub async fn transition_assignment_phase(
    pool: &SqlitePool,
    assignment_id: i64,
    actor_id: &str,
    actor_role: TransitionActorRole,
    target_phase: AssignmentPhase,
    expected_previous_phase: AssignmentPhase,
    event_id: i64,
) -> Result<HandAssignment, TransitionError> {
    transition_assignment_phase_inner(TransitionRequest {
        pool,
        assignment_id,
        actor_id,
        actor_role,
        target_phase,
        expected_previous_phase,
        event_id,
        override_role_check: false,
    })
    .await
}

/// Like [`transition_assignment_phase`] but with an explicit override flag
/// that allows Head/Director to bypass role-ownership checks.
pub async fn transition_assignment_phase_override(
    pool: &SqlitePool,
    assignment_id: i64,
    actor_id: &str,
    actor_role: TransitionActorRole,
    target_phase: AssignmentPhase,
    expected_previous_phase: AssignmentPhase,
    event_id: i64,
) -> Result<HandAssignment, TransitionError> {
    transition_assignment_phase_inner(TransitionRequest {
        pool,
        assignment_id,
        actor_id,
        actor_role,
        target_phase,
        expected_previous_phase,
        event_id,
        override_role_check: true,
    })
    .await
}

struct TransitionRequest<'a> {
    pool: &'a SqlitePool,
    assignment_id: i64,
    actor_id: &'a str,
    actor_role: TransitionActorRole,
    target_phase: AssignmentPhase,
    expected_previous_phase: AssignmentPhase,
    event_id: i64,
    override_role_check: bool,
}

async fn transition_assignment_phase_inner(
    req: TransitionRequest<'_>,
) -> Result<HandAssignment, TransitionError> {
    let TransitionRequest {
        pool,
        assignment_id,
        actor_id,
        actor_role,
        target_phase,
        expected_previous_phase,
        event_id,
        override_role_check,
    } = req;
    let mut tx = pool.begin().await?;

    // Read current assignment inside the transaction for consistency.
    let row = sqlx::query_as::<_, HandAssignment>("SELECT * FROM hand_assignments WHERE id = ?")
        .bind(assignment_id)
        .fetch_optional(&mut *tx)
        .await?;

    let assignment = row.ok_or(TransitionError::AssignmentNotFound { assignment_id })?;

    // Parse the current phase from the DB column.
    let current_phase =
        AssignmentPhase::from_str(assignment.assignment_phase.as_deref().unwrap_or("assigned"))
            .ok_or(TransitionError::IllegalTransition {
                from: "unknown",
                to: target_phase.as_str(),
            })?;

    // Pure validation (no side effects).
    validate_transition(
        current_phase,
        target_phase,
        expected_previous_phase,
        actor_role,
        override_role_check,
    )?;

    // Perform the phase UPDATE — this is the single sanctioned write path.
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE hand_assignments \
         SET assignment_phase = ?, \
             phase_changed_at = ?, \
             phase_changed_by = ?, \
             phase_actor_role = ?, \
             phase_event_id = ? \
         WHERE id = ?",
    )
    .bind(target_phase.as_str())
    .bind(&now)
    .bind(actor_id)
    .bind(actor_role.as_str())
    .bind(event_id)
    .bind(assignment_id)
    .execute(&mut *tx)
    .await?;

    // Re-read the updated row to return.
    let updated =
        sqlx::query_as::<_, HandAssignment>("SELECT * FROM hand_assignments WHERE id = ?")
            .bind(assignment_id)
            .fetch_one(&mut *tx)
            .await?;

    tx.commit().await?;

    Ok(updated)
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

    #[test]
    fn all_legal_transitions_are_covered() {
        // Ensure the transition map has exactly 8 entries matching the spec.
        assert_eq!(
            LEGAL_TRANSITIONS.len(),
            8,
            "legal transition map should have exactly 8 entries"
        );
    }
}
