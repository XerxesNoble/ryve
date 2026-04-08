// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Delegation contracts for the Ryve agent hierarchy.
//!
//! Ryve runs three nested layers of LLM-powered coding agents:
//!
//! ```text
//!   Director (Atlas) — the user-facing primary agent
//!       │  DirectorBrief
//!       ▼
//!     Head — Crew orchestrator
//!       │  HeadAssignment
//!       ▼
//!     Hand — single-spark worker
//!       │  HandReturn
//!       ▼
//!     Head — collects HandReturns into a HeadSynthesis
//!       │  HeadSynthesis
//!       ▼
//!   Director — relays synthesis back to the user
//! ```
//!
//! Each arrow above is a **delegation contract**: a serializable payload that
//! flows between roles. Contracts are intentionally stable, transport-agnostic
//! data shapes — they survive a process boundary because every delegation
//! step in Ryve crosses one (CLI invocation, subprocess spawn, or comment on
//! a spark).
//!
//! See `docs/DELEGATION_CONTRACTS.md` for the long-form description, the
//! motivation behind each field, and the wire format.
//!
//! This module defines the four contracts plus stub helpers used to
//! validate and round-trip them through JSON. Concrete transports (CLI
//! verbs, subprocess spawn, comment writes) are layered on top in
//! follow-up sparks. Until those follow-ups land, the types and helpers
//! are unused outside this module's tests, so dead_code is allowed.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// Role variant carried inside a delegation contract.
///
/// This is intentionally distinct from [`data::sparks::types::AgentRole`]:
/// the canonical hierarchy enum (Director / Head / Hand) describes *who*
/// an agent is in the workshop, while `ContractRole` is the *role on a
/// specific delegation hop* and additionally needs `Merger`, which the
/// canonical enum deliberately omits (Mergers are spawned as a
/// specialised Hand — see the `from_str("merger") == None` invariant in
/// `data::sparks::types`). Keeping the two types separate makes the
/// distinction explicit at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractRole {
    /// The user-facing primary agent. Currently Atlas. There is at most one
    /// active Director per workshop.
    Director,
    /// A Crew orchestrator. Spawned by the Director, owns one Crew, fans
    /// work out to Hands, and synthesises their results back up.
    Head,
    /// A single-spark worker. Spawned by a Head into its own git worktree.
    Hand,
    /// A specialised Hand whose only job is to integrate a Crew's worktree
    /// branches into a single PR. Surfaces here so a Head can include it in
    /// the same delegation flow as ordinary Hands.
    Merger,
}

impl ContractRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ContractRole::Director => "director",
            ContractRole::Head => "head",
            ContractRole::Hand => "hand",
            ContractRole::Merger => "merger",
        }
    }
}

/// Outcome of a delegated unit of work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationOutcome {
    /// All acceptance criteria satisfied; the spark was closed `completed`.
    Completed,
    /// Work could not proceed; details captured in the contract's notes.
    /// The spark is still open or marked `blocked`.
    Blocked,
    /// The recipient explicitly refused the assignment (e.g. out of scope,
    /// duplicate, ambiguous). Caller should re-plan.
    Declined,
    /// The recipient stopped reporting heartbeats and was reaped by the
    /// caller. Caller should re-spawn.
    Abandoned,
}

impl DelegationOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            DelegationOutcome::Completed => "completed",
            DelegationOutcome::Blocked => "blocked",
            DelegationOutcome::Declined => "declined",
            DelegationOutcome::Abandoned => "abandoned",
        }
    }
}

// ── Director → Head ────────────────────────────────────

/// `DirectorBrief` is what the Director hands to a newly-spawned Head when
/// it decides a user request is large enough to warrant a Crew.
///
/// The brief is the *what* and *why* — it never prescribes the *how*. The
/// Head is expected to read the workgraph, decompose the goal into sparks,
/// and pick agents per sub-task using its own judgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectorBrief {
    /// Stable id assigned by the Director so the eventual `HeadSynthesis`
    /// can be correlated back to the originating brief.
    pub brief_id: String,
    /// Plain-language statement of what the user wants accomplished.
    pub user_goal: String,
    /// Optional epic spark id the Head should attach all created child
    /// sparks to. If `None`, the Head must create its own parent epic.
    pub parent_epic_id: Option<String>,
    /// Constraints the Director is propagating from the user or the
    /// workshop (e.g. "do not touch the auth module", "must ship by
    /// Friday"). Non-empty constraints are mandatory inputs to the Head's
    /// decomposition.
    pub constraints: Vec<String>,
    /// Things the user explicitly does NOT want done. Mirrors a spark's
    /// non-goals so the Head does not over-scope.
    pub non_goals: Vec<String>,
    /// Optional success criterion in plain language. The Head MUST encode
    /// this as one or more `--acceptance` flags on the parent epic when it
    /// creates sparks.
    pub success_criterion: Option<String>,
}

impl DirectorBrief {
    pub fn new(brief_id: impl Into<String>, user_goal: impl Into<String>) -> Self {
        Self {
            brief_id: brief_id.into(),
            user_goal: user_goal.into(),
            parent_epic_id: None,
            constraints: Vec::new(),
            non_goals: Vec::new(),
            success_criterion: None,
        }
    }
}

// ── Head → Hand ────────────────────────────────────────

/// `HeadAssignment` is what a Head hands to a Hand when it spawns one via
/// `ryve hand spawn`. It is the on-the-wire form of a single delegation
/// from Head to Hand.
///
/// In the current implementation the fields here map directly onto the
/// arguments of `ryve hand spawn` plus the system-prompt content composed
/// by `agent_prompts::compose_hand_prompt`. Keeping the contract explicit
/// (rather than letting the CLI args be the only source of truth) lets the
/// Director and Head reason about delegations without shelling out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadAssignment {
    /// Spark the Hand will execute.
    pub spark_id: String,
    /// Crew the Hand will be enrolled in. `None` is permitted only for
    /// solo Hands spawned outside any Crew (rare; mostly the manual
    /// "+ → New Hand" UI path).
    pub crew_id: Option<String>,
    /// Coding agent CLI to invoke (`claude`, `codex`, `aider`, `opencode`).
    pub agent_command: String,
    /// Role on the Crew. Distinguishes ordinary Hands from the Merger.
    pub role: ContractRole,
    /// Brief id the assignment derives from, propagated for traceability.
    pub origin_brief_id: Option<String>,
}

impl HeadAssignment {
    pub fn new(spark_id: impl Into<String>, agent_command: impl Into<String>) -> Self {
        Self {
            spark_id: spark_id.into(),
            crew_id: None,
            agent_command: agent_command.into(),
            role: ContractRole::Hand,
            origin_brief_id: None,
        }
    }
}

// ── Hand → Head ────────────────────────────────────────

/// `HandReturn` is what a Hand sends back to its Head when it finishes (or
/// gives up on) a spark.
///
/// Today the canonical channel is a comment on the spark plus the spark's
/// closed status; the Head reads both via `ryve crew show` and `ryve spark
/// show`. This struct is the schema of that comment payload so the Head
/// can parse it programmatically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandReturn {
    /// Spark the Hand was working on.
    pub spark_id: String,
    /// Session id of the Hand reporting in.
    pub session_id: String,
    /// Outcome of the work.
    pub outcome: DelegationOutcome,
    /// Short human-readable summary of what the Hand did. Becomes the
    /// body of the comment posted on the spark.
    pub summary: String,
    /// Spark ids the Hand discovered while working that the Head should
    /// schedule as new work. Empty if no follow-ups.
    pub follow_up_sparks: Vec<String>,
    /// Git artifacts produced (commit shas, branch names, PR URLs). The
    /// Merger uses these to know what to integrate.
    pub artifacts: Vec<String>,
}

impl HandReturn {
    pub fn new(
        spark_id: impl Into<String>,
        session_id: impl Into<String>,
        outcome: DelegationOutcome,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            spark_id: spark_id.into(),
            session_id: session_id.into(),
            outcome,
            summary: summary.into(),
            follow_up_sparks: Vec::new(),
            artifacts: Vec::new(),
        }
    }
}

// ── Head → Director ────────────────────────────────────

/// `HeadSynthesis` is what a Head sends back to the Director once its
/// Crew has finished (or partially finished) the brief.
///
/// The Director uses the synthesis to compose its reply to the user. The
/// shape is intentionally narrow: a single overall outcome, the
/// per-spark roll-up, and one summary string. The Director — not the Head
/// — owns the user-facing rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadSynthesis {
    /// Brief id this synthesis answers.
    pub brief_id: String,
    /// Crew the Head was running.
    pub crew_id: String,
    /// Overall outcome of the brief, derived from the constituent
    /// `HandReturn`s.
    pub overall_outcome: DelegationOutcome,
    /// One short paragraph summarising the Crew's work suitable for the
    /// Director to relay to the user verbatim.
    pub summary: String,
    /// Per-Hand returns the synthesis is built from. Order is execution
    /// order so the Director can render a chronological recap.
    pub hand_returns: Vec<HandReturn>,
    /// Optional PR URL produced by the Crew's Merger, if any.
    pub pr_url: Option<String>,
    /// Sparks the Head escalates back to the Director as still requiring
    /// human input (blocked, declined, or follow-ups too large for the
    /// Crew to absorb).
    pub escalations: Vec<String>,
}

impl HeadSynthesis {
    pub fn new(
        brief_id: impl Into<String>,
        crew_id: impl Into<String>,
        overall_outcome: DelegationOutcome,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            brief_id: brief_id.into(),
            crew_id: crew_id.into(),
            overall_outcome,
            summary: summary.into(),
            hand_returns: Vec::new(),
            pr_url: None,
            escalations: Vec::new(),
        }
    }
}

// ── Stubbed transport helpers ──────────────────────────
//
// The four functions below are the only entry points the rest of the
// codebase should call when it wants to *send* a delegation contract.
// They are intentionally pure today: they validate the payload and
// return its JSON encoding. A follow-up spark will replace the bodies
// with the real transport (CLI dispatch, subprocess spawn, comment
// write) without changing the call sites.

/// Errors that can be raised when serialising or validating a contract.
#[derive(Debug, thiserror::Error)]
pub enum DelegationError {
    #[error("contract field `{0}` must not be empty")]
    EmptyField(&'static str),
    #[error("serialise failed: {0}")]
    Serialise(#[from] serde_json::Error),
}

/// Director → Head: validate and encode a `DirectorBrief` for transport
/// to a freshly-spawned Head.
pub fn delegate_to_head(brief: &DirectorBrief) -> Result<String, DelegationError> {
    if brief.brief_id.is_empty() {
        return Err(DelegationError::EmptyField("brief_id"));
    }
    if brief.user_goal.is_empty() {
        return Err(DelegationError::EmptyField("user_goal"));
    }
    Ok(serde_json::to_string(brief)?)
}

/// Head → Hand: validate and encode a `HeadAssignment` for the Hand
/// spawn path.
pub fn delegate_to_hand(assignment: &HeadAssignment) -> Result<String, DelegationError> {
    if assignment.spark_id.is_empty() {
        return Err(DelegationError::EmptyField("spark_id"));
    }
    if assignment.agent_command.is_empty() {
        return Err(DelegationError::EmptyField("agent_command"));
    }
    Ok(serde_json::to_string(assignment)?)
}

/// Hand → Head: validate and encode a `HandReturn` for posting back to
/// the Crew's Head.
pub fn return_to_head(report: &HandReturn) -> Result<String, DelegationError> {
    if report.spark_id.is_empty() {
        return Err(DelegationError::EmptyField("spark_id"));
    }
    if report.session_id.is_empty() {
        return Err(DelegationError::EmptyField("session_id"));
    }
    Ok(serde_json::to_string(report)?)
}

/// Head → Director: validate and encode a `HeadSynthesis` for relay to
/// the user-facing Director.
pub fn synthesise_for_director(synthesis: &HeadSynthesis) -> Result<String, DelegationError> {
    if synthesis.brief_id.is_empty() {
        return Err(DelegationError::EmptyField("brief_id"));
    }
    if synthesis.crew_id.is_empty() {
        return Err(DelegationError::EmptyField("crew_id"));
    }
    Ok(serde_json::to_string(synthesis)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn director_brief_round_trips_through_json() {
        let mut brief = DirectorBrief::new("br-1", "Add OAuth login");
        brief.parent_epic_id = Some("sp-epic-1".to_string());
        brief.constraints.push("must not touch billing".to_string());
        brief.non_goals.push("password reset".to_string());
        brief.success_criterion = Some("user can sign in with Google".to_string());

        let wire = delegate_to_head(&brief).expect("encode");
        let decoded: DirectorBrief = serde_json::from_str(&wire).expect("decode");
        assert_eq!(brief, decoded);
    }

    #[test]
    fn head_assignment_round_trips_through_json() {
        let mut assignment = HeadAssignment::new("sp-task-1", "claude");
        assignment.crew_id = Some("cr-1".to_string());
        assignment.role = ContractRole::Hand;
        assignment.origin_brief_id = Some("br-1".to_string());

        let wire = delegate_to_hand(&assignment).expect("encode");
        let decoded: HeadAssignment = serde_json::from_str(&wire).expect("decode");
        assert_eq!(assignment, decoded);
    }

    #[test]
    fn hand_return_round_trips_through_json() {
        let mut ret = HandReturn::new(
            "sp-task-1",
            "ses-1",
            DelegationOutcome::Completed,
            "implemented OAuth flow + tests",
        );
        ret.follow_up_sparks.push("sp-followup-1".to_string());
        ret.artifacts.push("commit:abc1234".to_string());

        let wire = return_to_head(&ret).expect("encode");
        let decoded: HandReturn = serde_json::from_str(&wire).expect("decode");
        assert_eq!(ret, decoded);
    }

    #[test]
    fn head_synthesis_round_trips_through_json() {
        let inner = HandReturn::new("sp-task-1", "ses-1", DelegationOutcome::Completed, "done");
        let mut syn = HeadSynthesis::new(
            "br-1",
            "cr-1",
            DelegationOutcome::Completed,
            "Crew shipped OAuth.",
        );
        syn.hand_returns.push(inner);
        syn.pr_url = Some("https://github.com/o/r/pull/42".to_string());
        syn.escalations.push("sp-followup-1".to_string());

        let wire = synthesise_for_director(&syn).expect("encode");
        let decoded: HeadSynthesis = serde_json::from_str(&wire).expect("decode");
        assert_eq!(syn, decoded);
    }

    #[test]
    fn empty_required_fields_are_rejected() {
        let brief = DirectorBrief::new("", "goal");
        assert!(matches!(
            delegate_to_head(&brief),
            Err(DelegationError::EmptyField("brief_id"))
        ));

        let brief = DirectorBrief::new("br-1", "");
        assert!(matches!(
            delegate_to_head(&brief),
            Err(DelegationError::EmptyField("user_goal"))
        ));

        let assignment = HeadAssignment::new("", "claude");
        assert!(matches!(
            delegate_to_hand(&assignment),
            Err(DelegationError::EmptyField("spark_id"))
        ));

        let assignment = HeadAssignment::new("sp-1", "");
        assert!(matches!(
            delegate_to_hand(&assignment),
            Err(DelegationError::EmptyField("agent_command"))
        ));

        let ret = HandReturn::new("", "ses", DelegationOutcome::Completed, "x");
        assert!(matches!(
            return_to_head(&ret),
            Err(DelegationError::EmptyField("spark_id"))
        ));

        let ret = HandReturn::new("sp", "", DelegationOutcome::Completed, "x");
        assert!(matches!(
            return_to_head(&ret),
            Err(DelegationError::EmptyField("session_id"))
        ));

        let syn = HeadSynthesis::new("", "cr", DelegationOutcome::Completed, "x");
        assert!(matches!(
            synthesise_for_director(&syn),
            Err(DelegationError::EmptyField("brief_id"))
        ));

        let syn = HeadSynthesis::new("br", "", DelegationOutcome::Completed, "x");
        assert!(matches!(
            synthesise_for_director(&syn),
            Err(DelegationError::EmptyField("crew_id"))
        ));
    }

    #[test]
    fn agent_role_string_form_is_stable() {
        assert_eq!(ContractRole::Director.as_str(), "director");
        assert_eq!(ContractRole::Head.as_str(), "head");
        assert_eq!(ContractRole::Hand.as_str(), "hand");
        assert_eq!(ContractRole::Merger.as_str(), "merger");
    }

    #[test]
    fn delegation_outcome_string_form_is_stable() {
        assert_eq!(DelegationOutcome::Completed.as_str(), "completed");
        assert_eq!(DelegationOutcome::Blocked.as_str(), "blocked");
        assert_eq!(DelegationOutcome::Declined.as_str(), "declined");
        assert_eq!(DelegationOutcome::Abandoned.as_str(), "abandoned");
    }
}
