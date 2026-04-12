// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Assignment phase state machine.
//!
//! An assignment progresses through 8 phases from `Assigned` to `Merged`.
//! Each transition is gated by the caller's role — only specific roles may
//! trigger specific edges. Head and Director can force any transition via
//! the `override` flag.
//!
//! Happy path:
//!   Assigned → InProgress → AwaitingReview → Approved → Merging → Merged
//!
//! Rejection loop:
//!   AwaitingReview → Rejected → InRepair → AwaitingReview

use std::fmt;

/// The 8 phases of an assignment's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssignmentPhase {
    Assigned,
    InProgress,
    AwaitingReview,
    Rejected,
    InRepair,
    Approved,
    Merging,
    Merged,
}

impl fmt::Display for AssignmentPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AssignmentPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Assigned => "assigned",
            Self::InProgress => "in_progress",
            Self::AwaitingReview => "awaiting_review",
            Self::Rejected => "rejected",
            Self::InRepair => "in_repair",
            Self::Approved => "approved",
            Self::Merging => "merging",
            Self::Merged => "merged",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "assigned" => Some(Self::Assigned),
            "in_progress" => Some(Self::InProgress),
            "awaiting_review" => Some(Self::AwaitingReview),
            "rejected" => Some(Self::Rejected),
            "in_repair" => Some(Self::InRepair),
            "approved" => Some(Self::Approved),
            "merging" => Some(Self::Merging),
            "merged" => Some(Self::Merged),
            _ => None,
        }
    }
}

/// Role of the actor attempting a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransitionRole {
    Hand,
    ReviewerHand,
    MergeHand,
    Head,
    Director,
}

impl TransitionRole {
    /// Head and Director sit above Hands in the hierarchy and can override.
    pub fn can_override(&self) -> bool {
        matches!(self, Self::Head | Self::Director)
    }
}

/// A request to advance an assignment from one phase to another.
#[derive(Debug)]
pub struct TransitionRequest {
    pub from: AssignmentPhase,
    pub to: AssignmentPhase,
    pub role: TransitionRole,
    /// When true, Head/Director may force any transition regardless of the
    /// normal edge map.
    pub override_flag: bool,
    /// If set, the caller asserts that the assignment is currently in this
    /// phase. A mismatch (i.e. `expected != from`) produces
    /// `TransitionError::PhaseMismatch`.
    pub expected_previous_phase: Option<AssignmentPhase>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TransitionError {
    /// The requested edge is not in the legal transition map.
    IllegalTransition {
        from: AssignmentPhase,
        to: AssignmentPhase,
    },
    /// The caller's role is not permitted for this transition.
    RoleNotPermitted {
        role: TransitionRole,
        from: AssignmentPhase,
        to: AssignmentPhase,
    },
    /// The `expected_previous_phase` did not match the actual `from` phase.
    PhaseMismatch {
        expected: AssignmentPhase,
        actual: AssignmentPhase,
    },
    /// Override was requested but the role cannot override.
    OverrideNotAllowed { role: TransitionRole },
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IllegalTransition { from, to } => {
                write!(f, "illegal transition: {from} → {to}")
            }
            Self::RoleNotPermitted { role, from, to } => {
                write!(f, "{role:?} cannot transition {from} → {to}")
            }
            Self::PhaseMismatch { expected, actual } => {
                write!(f, "phase mismatch: expected {expected}, actual {actual}")
            }
            Self::OverrideNotAllowed { role } => {
                write!(f, "override not allowed for role {role:?}")
            }
        }
    }
}

impl std::error::Error for TransitionError {}

/// Legal edges and the roles that may trigger them.
///
/// Returns `Some(allowed_roles)` if the `(from, to)` pair is a legal edge,
/// `None` otherwise.
fn legal_edge(from: AssignmentPhase, to: AssignmentPhase) -> Option<&'static [TransitionRole]> {
    use AssignmentPhase::*;
    use TransitionRole::*;

    match (from, to) {
        // Happy path
        (Assigned, InProgress) => Some(&[Hand, Head, Director]),
        (InProgress, AwaitingReview) => Some(&[Hand, Head, Director]),
        (AwaitingReview, Approved) => Some(&[ReviewerHand, Head, Director]),
        (Approved, Merging) => Some(&[MergeHand, Head, Director]),
        (Merging, Merged) => Some(&[MergeHand, Head, Director]),
        // Rejection loop
        (AwaitingReview, Rejected) => Some(&[ReviewerHand, Head, Director]),
        (Rejected, InRepair) => Some(&[Hand, Head, Director]),
        (InRepair, AwaitingReview) => Some(&[Hand, Head, Director]),
        _ => None,
    }
}

/// Attempt a phase transition. Returns the new phase on success.
pub fn transition(req: &TransitionRequest) -> Result<AssignmentPhase, TransitionError> {
    // 1. Check expected_previous_phase (out-of-order replay guard).
    if let Some(expected) = req.expected_previous_phase.filter(|&e| e != req.from) {
        return Err(TransitionError::PhaseMismatch {
            expected,
            actual: req.from,
        });
    }

    // 2. Override path — Head/Director can force any transition.
    if req.override_flag {
        if req.role.can_override() {
            return Ok(req.to);
        }
        return Err(TransitionError::OverrideNotAllowed { role: req.role });
    }

    // 3. Check that the edge exists.
    let allowed = legal_edge(req.from, req.to).ok_or(TransitionError::IllegalTransition {
        from: req.from,
        to: req.to,
    })?;

    // 4. Check that the role is permitted.
    if !allowed.contains(&req.role) {
        return Err(TransitionError::RoleNotPermitted {
            role: req.role,
            from: req.from,
            to: req.to,
        });
    }

    Ok(req.to)
}

#[cfg(test)]
mod tests {
    use AssignmentPhase::*;
    use TransitionRole::*;

    use super::*;

    /// Helper to build a simple transition request without override or
    /// expected_previous_phase.
    fn req(from: AssignmentPhase, to: AssignmentPhase, role: TransitionRole) -> TransitionRequest {
        TransitionRequest {
            from,
            to,
            role,
            override_flag: false,
            expected_previous_phase: None,
        }
    }

    // ── Happy-path legal transitions ─────────────────────

    #[test]
    fn legal_assigned_to_in_progress() {
        let r = req(Assigned, InProgress, Hand);
        assert_eq!(transition(&r).unwrap(), InProgress);
    }

    #[test]
    fn legal_in_progress_to_awaiting_review() {
        let r = req(InProgress, AwaitingReview, Hand);
        assert_eq!(transition(&r).unwrap(), AwaitingReview);
    }

    #[test]
    fn legal_awaiting_review_to_approved() {
        let r = req(AwaitingReview, Approved, ReviewerHand);
        assert_eq!(transition(&r).unwrap(), Approved);
    }

    #[test]
    fn legal_approved_to_merging() {
        let r = req(Approved, Merging, MergeHand);
        assert_eq!(transition(&r).unwrap(), Merging);
    }

    #[test]
    fn legal_merging_to_merged() {
        let r = req(Merging, Merged, MergeHand);
        assert_eq!(transition(&r).unwrap(), Merged);
    }

    // ── Rejection loop ───────────────────────────────────

    #[test]
    fn legal_awaiting_review_to_rejected() {
        let r = req(AwaitingReview, Rejected, ReviewerHand);
        assert_eq!(transition(&r).unwrap(), Rejected);
    }

    #[test]
    fn legal_rejected_to_in_repair() {
        let r = req(Rejected, InRepair, Hand);
        assert_eq!(transition(&r).unwrap(), InRepair);
    }

    #[test]
    fn legal_in_repair_to_awaiting_review() {
        let r = req(InRepair, AwaitingReview, Hand);
        assert_eq!(transition(&r).unwrap(), AwaitingReview);
    }

    // ── Full happy path end-to-end ───────────────────────

    #[test]
    fn full_happy_path() {
        let steps = [
            (Assigned, InProgress, Hand),
            (InProgress, AwaitingReview, Hand),
            (AwaitingReview, Approved, ReviewerHand),
            (Approved, Merging, MergeHand),
            (Merging, Merged, MergeHand),
        ];
        let mut phase = Assigned;
        for (from, to, role) in steps {
            assert_eq!(phase, from);
            phase = transition(&req(from, to, role)).unwrap();
            assert_eq!(phase, to);
        }
        assert_eq!(phase, Merged);
    }

    // ── Full rejection loop end-to-end ───────────────────

    #[test]
    fn rejection_loop_then_approve() {
        let mut phase = AwaitingReview;
        // Reject
        phase = transition(&req(phase, Rejected, ReviewerHand)).unwrap();
        assert_eq!(phase, Rejected);
        // Repair
        phase = transition(&req(phase, InRepair, Hand)).unwrap();
        assert_eq!(phase, InRepair);
        // Re-submit
        phase = transition(&req(phase, AwaitingReview, Hand)).unwrap();
        assert_eq!(phase, AwaitingReview);
        // Now approve
        phase = transition(&req(phase, Approved, ReviewerHand)).unwrap();
        assert_eq!(phase, Approved);
    }

    // ── Illegal transitions ──────────────────────────────

    #[test]
    fn illegal_in_progress_to_merged() {
        let r = req(InProgress, Merged, Hand);
        assert_eq!(
            transition(&r).unwrap_err(),
            TransitionError::IllegalTransition {
                from: InProgress,
                to: Merged,
            }
        );
    }

    #[test]
    fn illegal_assigned_to_approved() {
        let r = req(Assigned, Approved, ReviewerHand);
        assert_eq!(
            transition(&r).unwrap_err(),
            TransitionError::IllegalTransition {
                from: Assigned,
                to: Approved,
            }
        );
    }

    #[test]
    fn illegal_merged_to_anything() {
        for target in [
            Assigned,
            InProgress,
            AwaitingReview,
            Rejected,
            InRepair,
            Approved,
            Merging,
        ] {
            let r = req(Merged, target, Director);
            assert!(
                matches!(
                    transition(&r).unwrap_err(),
                    TransitionError::IllegalTransition { .. }
                ),
                "Merged → {target:?} should be illegal"
            );
        }
    }

    #[test]
    fn illegal_assigned_to_awaiting_review() {
        let r = req(Assigned, AwaitingReview, Hand);
        assert!(matches!(
            transition(&r).unwrap_err(),
            TransitionError::IllegalTransition { .. }
        ));
    }

    #[test]
    fn illegal_approved_to_in_progress() {
        let r = req(Approved, InProgress, Hand);
        assert!(matches!(
            transition(&r).unwrap_err(),
            TransitionError::IllegalTransition { .. }
        ));
    }

    #[test]
    fn illegal_rejected_to_approved() {
        let r = req(Rejected, Approved, ReviewerHand);
        assert!(matches!(
            transition(&r).unwrap_err(),
            TransitionError::IllegalTransition { .. }
        ));
    }

    // ── Role boundary tests ──────────────────────────────

    #[test]
    fn hand_cannot_approve() {
        let r = req(AwaitingReview, Approved, Hand);
        assert_eq!(
            transition(&r).unwrap_err(),
            TransitionError::RoleNotPermitted {
                role: Hand,
                from: AwaitingReview,
                to: Approved,
            }
        );
    }

    #[test]
    fn reviewer_hand_cannot_merge() {
        let r = req(Merging, Merged, ReviewerHand);
        assert_eq!(
            transition(&r).unwrap_err(),
            TransitionError::RoleNotPermitted {
                role: ReviewerHand,
                from: Merging,
                to: Merged,
            }
        );
    }

    #[test]
    fn merge_hand_cannot_reject() {
        let r = req(AwaitingReview, Rejected, MergeHand);
        assert_eq!(
            transition(&r).unwrap_err(),
            TransitionError::RoleNotPermitted {
                role: MergeHand,
                from: AwaitingReview,
                to: Rejected,
            }
        );
    }

    #[test]
    fn hand_cannot_start_merging() {
        let r = req(Approved, Merging, Hand);
        assert_eq!(
            transition(&r).unwrap_err(),
            TransitionError::RoleNotPermitted {
                role: Hand,
                from: Approved,
                to: Merging,
            }
        );
    }

    #[test]
    fn reviewer_hand_cannot_start_work() {
        let r = req(Assigned, InProgress, ReviewerHand);
        assert_eq!(
            transition(&r).unwrap_err(),
            TransitionError::RoleNotPermitted {
                role: ReviewerHand,
                from: Assigned,
                to: InProgress,
            }
        );
    }

    // ── Override flag tests ──────────────────────────────

    #[test]
    fn head_can_override_any_transition() {
        // Force an illegal edge: Assigned → Merged
        let r = TransitionRequest {
            from: Assigned,
            to: Merged,
            role: Head,
            override_flag: true,
            expected_previous_phase: None,
        };
        assert_eq!(transition(&r).unwrap(), Merged);
    }

    #[test]
    fn director_can_override_any_transition() {
        let r = TransitionRequest {
            from: InProgress,
            to: Merged,
            role: Director,
            override_flag: true,
            expected_previous_phase: None,
        };
        assert_eq!(transition(&r).unwrap(), Merged);
    }

    #[test]
    fn hand_cannot_override() {
        let r = TransitionRequest {
            from: Assigned,
            to: Merged,
            role: Hand,
            override_flag: true,
            expected_previous_phase: None,
        };
        assert_eq!(
            transition(&r).unwrap_err(),
            TransitionError::OverrideNotAllowed { role: Hand }
        );
    }

    #[test]
    fn reviewer_hand_cannot_override() {
        let r = TransitionRequest {
            from: Assigned,
            to: Merged,
            role: ReviewerHand,
            override_flag: true,
            expected_previous_phase: None,
        };
        assert_eq!(
            transition(&r).unwrap_err(),
            TransitionError::OverrideNotAllowed { role: ReviewerHand }
        );
    }

    #[test]
    fn merge_hand_cannot_override() {
        let r = TransitionRequest {
            from: Assigned,
            to: Merged,
            role: MergeHand,
            override_flag: true,
            expected_previous_phase: None,
        };
        assert_eq!(
            transition(&r).unwrap_err(),
            TransitionError::OverrideNotAllowed { role: MergeHand }
        );
    }

    // ── Out-of-order replay (expected_previous_phase) ────

    #[test]
    fn expected_previous_phase_mismatch_returns_error() {
        let r = TransitionRequest {
            from: InProgress,
            to: AwaitingReview,
            role: Hand,
            override_flag: false,
            expected_previous_phase: Some(Assigned),
        };
        assert_eq!(
            transition(&r).unwrap_err(),
            TransitionError::PhaseMismatch {
                expected: Assigned,
                actual: InProgress,
            }
        );
    }

    #[test]
    fn expected_previous_phase_match_succeeds() {
        let r = TransitionRequest {
            from: InProgress,
            to: AwaitingReview,
            role: Hand,
            override_flag: false,
            expected_previous_phase: Some(InProgress),
        };
        assert_eq!(transition(&r).unwrap(), AwaitingReview);
    }

    #[test]
    fn phase_mismatch_checked_before_override() {
        // Even with override, a phase mismatch should fail first.
        let r = TransitionRequest {
            from: InProgress,
            to: Merged,
            role: Director,
            override_flag: true,
            expected_previous_phase: Some(Assigned),
        };
        assert!(matches!(
            transition(&r).unwrap_err(),
            TransitionError::PhaseMismatch { .. }
        ));
    }
}
