// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Pure event-sourced projector: `project(&[Event]) -> WorldState`.
//!
//! The projector rebuilds the full assignment state from an ordered event
//! stream with no I/O and no database access. Replay is deterministic:
//! the same event sequence always produces byte-identical `WorldState`.
//!
//! # Idempotency
//!
//! Each event carries a unique `event_id`. Applying the same event twice is
//! a no-op: the projector tracks the set of applied `event_id`s and skips
//! duplicates. This lets the write path emit an event, fail mid-delivery,
//! and retry without the replay diverging from the live state.
//!
//! # Scope
//!
//! The `Event` enum and `WorldState` shape live here because the projector
//! is the authoritative consumer. The write path, the outbox relay, and the
//! replay test all serialize events against these types. Any new event
//! variant must be reflected in `apply_event` and gated by
//! `schema_version` in the write path.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::types::{AssignmentLiveness, AssignmentPhase};

/// Schema version of the current event payload format. Events written at
/// older versions must be migrated before they reach the projector; the
/// projector itself only speaks the current version.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// One durable event in the outbox. Variant discriminator is the JSON
/// `"type"` tag; payload fields are flattened alongside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    AssignmentCreated {
        event_id: String,
        schema_version: u32,
        timestamp: String,
        assignment_id: String,
        actor_id: String,
        spark_id: String,
        initial_phase: AssignmentPhase,
        source_branch: Option<String>,
        target_branch: Option<String>,
    },
    PhaseTransitioned {
        event_id: String,
        schema_version: u32,
        timestamp: String,
        assignment_id: String,
        actor_id: String,
        from_phase: AssignmentPhase,
        to_phase: AssignmentPhase,
    },
    HeartbeatReceived {
        event_id: String,
        schema_version: u32,
        timestamp: String,
        assignment_id: String,
        actor_id: String,
    },
    /// The watchdog observed a liveness change on an active assignment.
    /// Emitted once per transition (Healthy→AtRisk→Stuck, and back) so the
    /// outbox relay surfaces the Stuck edge on IRC and the projector can
    /// keep `AssignmentView::liveness` in sync without re-reading the DB.
    /// Parent epic `ryve-cf05fd85`.
    LivenessTransitioned {
        event_id: String,
        schema_version: u32,
        timestamp: String,
        assignment_id: String,
        actor_id: String,
        from_liveness: AssignmentLiveness,
        to_liveness: AssignmentLiveness,
    },
    ReviewRequested {
        event_id: String,
        schema_version: u32,
        timestamp: String,
        assignment_id: String,
        actor_id: String,
    },
    ReviewCompleted {
        event_id: String,
        schema_version: u32,
        timestamp: String,
        assignment_id: String,
        actor_id: String,
        approved: bool,
    },
    MergePreconditionFailed {
        event_id: String,
        schema_version: u32,
        timestamp: String,
        assignment_id: String,
        actor_id: String,
        reason: String,
    },
    MergeCompleted {
        event_id: String,
        schema_version: u32,
        timestamp: String,
        assignment_id: String,
        actor_id: String,
    },
}

impl Event {
    pub fn event_id(&self) -> &str {
        match self {
            Event::AssignmentCreated { event_id, .. }
            | Event::PhaseTransitioned { event_id, .. }
            | Event::HeartbeatReceived { event_id, .. }
            | Event::LivenessTransitioned { event_id, .. }
            | Event::ReviewRequested { event_id, .. }
            | Event::ReviewCompleted { event_id, .. }
            | Event::MergePreconditionFailed { event_id, .. }
            | Event::MergeCompleted { event_id, .. } => event_id,
        }
    }

    pub fn schema_version(&self) -> u32 {
        match self {
            Event::AssignmentCreated { schema_version, .. }
            | Event::PhaseTransitioned { schema_version, .. }
            | Event::HeartbeatReceived { schema_version, .. }
            | Event::LivenessTransitioned { schema_version, .. }
            | Event::ReviewRequested { schema_version, .. }
            | Event::ReviewCompleted { schema_version, .. }
            | Event::MergePreconditionFailed { schema_version, .. }
            | Event::MergeCompleted { schema_version, .. } => *schema_version,
        }
    }

    pub fn assignment_id(&self) -> &str {
        match self {
            Event::AssignmentCreated { assignment_id, .. }
            | Event::PhaseTransitioned { assignment_id, .. }
            | Event::HeartbeatReceived { assignment_id, .. }
            | Event::LivenessTransitioned { assignment_id, .. }
            | Event::ReviewRequested { assignment_id, .. }
            | Event::ReviewCompleted { assignment_id, .. }
            | Event::MergePreconditionFailed { assignment_id, .. }
            | Event::MergeCompleted { assignment_id, .. } => assignment_id,
        }
    }

    pub fn timestamp(&self) -> &str {
        match self {
            Event::AssignmentCreated { timestamp, .. }
            | Event::PhaseTransitioned { timestamp, .. }
            | Event::HeartbeatReceived { timestamp, .. }
            | Event::LivenessTransitioned { timestamp, .. }
            | Event::ReviewRequested { timestamp, .. }
            | Event::ReviewCompleted { timestamp, .. }
            | Event::MergePreconditionFailed { timestamp, .. }
            | Event::MergeCompleted { timestamp, .. } => timestamp,
        }
    }
}

/// Outcome of the last review decision on an assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOutcome {
    Approved,
    Rejected,
}

/// Derived view of one assignment, rebuilt from its event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignmentView {
    pub assignment_id: String,
    pub spark_id: String,
    pub actor_id: String,
    pub phase: AssignmentPhase,
    pub source_branch: Option<String>,
    pub target_branch: Option<String>,
    pub event_version: u64,
    pub created_at: String,
    pub updated_at: String,
    pub last_heartbeat_at: Option<String>,
    /// Derived liveness per the watchdog. Starts `Healthy` on creation and
    /// advances via [`Event::LivenessTransitioned`]. Parent epic
    /// `ryve-cf05fd85`.
    pub liveness: AssignmentLiveness,
    /// How many times this assignment has re-entered the repair loop. The
    /// projector increments this on every `PhaseTransitioned` event whose
    /// (from, to) is (Rejected, InRepair); the write path pairs the same
    /// increment with a DB UPDATE so live and replayed state stay in sync.
    /// Crossing the workshop's `repair_cycle_limit` escalates the
    /// assignment to [`AssignmentLiveness::Stuck`] via a canonical
    /// `LivenessTransitioned` event.
    pub repair_cycle_count: i64,
    pub last_review_outcome: Option<ReviewOutcome>,
    pub last_review_at: Option<String>,
    pub last_merge_precondition_failure: Option<String>,
    pub merged_at: Option<String>,
}

/// Full projected workshop state. `BTreeMap` gives a deterministic
/// iteration order, so serializing this struct yields byte-identical
/// output across replays.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldState {
    pub assignments: BTreeMap<String, AssignmentView>,
}

/// Rebuild `WorldState` from an ordered event stream.
///
/// Applied in input order. Duplicate `event_id`s are skipped — applying
/// the same event twice is a no-op. Events that reference an unknown
/// assignment (i.e. arriving before the corresponding `AssignmentCreated`)
/// are silently dropped; a well-formed stream from the outbox never
/// produces them.
pub fn project(events: &[Event]) -> WorldState {
    let mut state = WorldState::default();
    let mut applied: HashSet<String> = HashSet::new();

    for event in events {
        if !applied.insert(event.event_id().to_string()) {
            continue;
        }
        apply_event(&mut state, event);
    }

    state
}

fn apply_event(state: &mut WorldState, event: &Event) {
    match event {
        Event::AssignmentCreated {
            timestamp,
            assignment_id,
            actor_id,
            spark_id,
            initial_phase,
            source_branch,
            target_branch,
            ..
        } => {
            // First create wins; later re-creations are ignored so replay
            // stays deterministic even if the outbox is misused.
            state
                .assignments
                .entry(assignment_id.clone())
                .or_insert_with(|| AssignmentView {
                    assignment_id: assignment_id.clone(),
                    spark_id: spark_id.clone(),
                    actor_id: actor_id.clone(),
                    phase: *initial_phase,
                    source_branch: source_branch.clone(),
                    target_branch: target_branch.clone(),
                    event_version: 1,
                    created_at: timestamp.clone(),
                    updated_at: timestamp.clone(),
                    last_heartbeat_at: None,
                    liveness: AssignmentLiveness::Healthy,
                    repair_cycle_count: 0,
                    last_review_outcome: None,
                    last_review_at: None,
                    last_merge_precondition_failure: None,
                    merged_at: None,
                });
        }
        Event::PhaseTransitioned {
            timestamp,
            assignment_id,
            from_phase,
            to_phase,
            ..
        } => {
            if let Some(view) = state.assignments.get_mut(assignment_id) {
                view.phase = *to_phase;
                if *from_phase == AssignmentPhase::Rejected
                    && *to_phase == AssignmentPhase::InRepair
                {
                    view.repair_cycle_count += 1;
                }
                view.event_version += 1;
                view.updated_at = timestamp.clone();
            }
        }
        Event::HeartbeatReceived {
            timestamp,
            assignment_id,
            ..
        } => {
            if let Some(view) = state.assignments.get_mut(assignment_id) {
                view.last_heartbeat_at = Some(timestamp.clone());
                view.event_version += 1;
                view.updated_at = timestamp.clone();
            }
        }
        Event::LivenessTransitioned {
            timestamp,
            assignment_id,
            to_liveness,
            ..
        } => {
            if let Some(view) = state.assignments.get_mut(assignment_id) {
                view.liveness = *to_liveness;
                view.event_version += 1;
                view.updated_at = timestamp.clone();
            }
        }
        Event::ReviewRequested {
            timestamp,
            assignment_id,
            ..
        } => {
            if let Some(view) = state.assignments.get_mut(assignment_id) {
                view.event_version += 1;
                view.updated_at = timestamp.clone();
            }
        }
        Event::ReviewCompleted {
            timestamp,
            assignment_id,
            approved,
            ..
        } => {
            if let Some(view) = state.assignments.get_mut(assignment_id) {
                view.last_review_outcome = Some(if *approved {
                    ReviewOutcome::Approved
                } else {
                    ReviewOutcome::Rejected
                });
                view.last_review_at = Some(timestamp.clone());
                view.event_version += 1;
                view.updated_at = timestamp.clone();
            }
        }
        Event::MergePreconditionFailed {
            timestamp,
            assignment_id,
            reason,
            ..
        } => {
            if let Some(view) = state.assignments.get_mut(assignment_id) {
                view.last_merge_precondition_failure = Some(reason.clone());
                view.event_version += 1;
                view.updated_at = timestamp.clone();
            }
        }
        Event::MergeCompleted {
            timestamp,
            assignment_id,
            ..
        } => {
            if let Some(view) = state.assignments.get_mut(assignment_id) {
                view.merged_at = Some(timestamp.clone());
                view.event_version += 1;
                view.updated_at = timestamp.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn created(id: &str, ts: &str, assignment_id: &str) -> Event {
        Event::AssignmentCreated {
            event_id: id.to_string(),
            schema_version: CURRENT_SCHEMA_VERSION,
            timestamp: ts.to_string(),
            assignment_id: assignment_id.to_string(),
            actor_id: "actor-1".to_string(),
            spark_id: "sp-1".to_string(),
            initial_phase: AssignmentPhase::Assigned,
            source_branch: Some("hand/abc".to_string()),
            target_branch: Some("main".to_string()),
        }
    }

    fn transitioned(
        id: &str,
        ts: &str,
        assignment_id: &str,
        from: AssignmentPhase,
        to: AssignmentPhase,
    ) -> Event {
        Event::PhaseTransitioned {
            event_id: id.to_string(),
            schema_version: CURRENT_SCHEMA_VERSION,
            timestamp: ts.to_string(),
            assignment_id: assignment_id.to_string(),
            actor_id: "actor-1".to_string(),
            from_phase: from,
            to_phase: to,
        }
    }

    #[test]
    fn empty_event_stream_yields_empty_state() {
        let state = project(&[]);
        assert!(state.assignments.is_empty());
    }

    #[test]
    fn assignment_created_is_projected() {
        let events = vec![created("e1", "2026-04-16T00:00:00Z", "asgn-1")];
        let state = project(&events);

        let view = state.assignments.get("asgn-1").expect("assignment exists");
        assert_eq!(view.phase, AssignmentPhase::Assigned);
        assert_eq!(view.event_version, 1);
        assert_eq!(view.created_at, "2026-04-16T00:00:00Z");
        assert_eq!(view.updated_at, "2026-04-16T00:00:00Z");
    }

    #[test]
    fn phase_transitions_advance_state() {
        let events = vec![
            created("e1", "2026-04-16T00:00:00Z", "asgn-1"),
            transitioned(
                "e2",
                "2026-04-16T00:01:00Z",
                "asgn-1",
                AssignmentPhase::Assigned,
                AssignmentPhase::InProgress,
            ),
            transitioned(
                "e3",
                "2026-04-16T00:02:00Z",
                "asgn-1",
                AssignmentPhase::InProgress,
                AssignmentPhase::AwaitingReview,
            ),
        ];
        let state = project(&events);

        let view = &state.assignments["asgn-1"];
        assert_eq!(view.phase, AssignmentPhase::AwaitingReview);
        assert_eq!(view.event_version, 3);
        assert_eq!(view.updated_at, "2026-04-16T00:02:00Z");
    }

    #[test]
    fn duplicate_event_ids_are_idempotent() {
        let events = vec![
            created("e1", "2026-04-16T00:00:00Z", "asgn-1"),
            transitioned(
                "e2",
                "2026-04-16T00:01:00Z",
                "asgn-1",
                AssignmentPhase::Assigned,
                AssignmentPhase::InProgress,
            ),
        ];
        let once = project(&events);

        // Double-apply the same events — final state must match.
        let doubled: Vec<Event> = events.iter().chain(events.iter()).cloned().collect();
        let twice = project(&doubled);

        assert_eq!(once, twice, "duplicate events must be no-ops");
        assert_eq!(twice.assignments["asgn-1"].event_version, 2);
    }

    #[test]
    fn events_for_unknown_assignment_are_dropped() {
        let events = vec![transitioned(
            "e1",
            "2026-04-16T00:00:00Z",
            "asgn-missing",
            AssignmentPhase::Assigned,
            AssignmentPhase::InProgress,
        )];
        let state = project(&events);
        assert!(state.assignments.is_empty());
    }

    #[test]
    fn heartbeat_updates_last_heartbeat() {
        let events = vec![
            created("e1", "2026-04-16T00:00:00Z", "asgn-1"),
            Event::HeartbeatReceived {
                event_id: "e2".to_string(),
                schema_version: CURRENT_SCHEMA_VERSION,
                timestamp: "2026-04-16T00:05:00Z".to_string(),
                assignment_id: "asgn-1".to_string(),
                actor_id: "actor-1".to_string(),
            },
        ];
        let state = project(&events);
        assert_eq!(
            state.assignments["asgn-1"].last_heartbeat_at.as_deref(),
            Some("2026-04-16T00:05:00Z"),
        );
    }

    #[test]
    fn liveness_transitioned_updates_assignment_liveness() {
        // Parent epic ryve-cf05fd85 / watchdog spark ryve-fe4e03d3: the
        // projector must replay the watchdog's liveness edges so the
        // derived view stays in sync with the DB-persisted `liveness`
        // column. Starts Healthy, walks to AtRisk then Stuck.
        let events = vec![
            created("e1", "2026-04-16T00:00:00Z", "asgn-1"),
            Event::LivenessTransitioned {
                event_id: "e2".to_string(),
                schema_version: CURRENT_SCHEMA_VERSION,
                timestamp: "2026-04-16T00:01:00Z".to_string(),
                assignment_id: "asgn-1".to_string(),
                actor_id: "watchdog".to_string(),
                from_liveness: AssignmentLiveness::Healthy,
                to_liveness: AssignmentLiveness::AtRisk,
            },
            Event::LivenessTransitioned {
                event_id: "e3".to_string(),
                schema_version: CURRENT_SCHEMA_VERSION,
                timestamp: "2026-04-16T00:02:00Z".to_string(),
                assignment_id: "asgn-1".to_string(),
                actor_id: "watchdog".to_string(),
                from_liveness: AssignmentLiveness::AtRisk,
                to_liveness: AssignmentLiveness::Stuck,
            },
        ];
        let state = project(&events);
        let view = &state.assignments["asgn-1"];
        assert_eq!(view.liveness, AssignmentLiveness::Stuck);
        assert_eq!(view.event_version, 3);
        assert_eq!(view.updated_at, "2026-04-16T00:02:00Z");
    }

    #[test]
    fn review_completed_records_outcome() {
        let events = vec![
            created("e1", "2026-04-16T00:00:00Z", "asgn-1"),
            Event::ReviewCompleted {
                event_id: "e2".to_string(),
                schema_version: CURRENT_SCHEMA_VERSION,
                timestamp: "2026-04-16T00:10:00Z".to_string(),
                assignment_id: "asgn-1".to_string(),
                actor_id: "reviewer-1".to_string(),
                approved: false,
            },
        ];
        let state = project(&events);
        assert_eq!(
            state.assignments["asgn-1"].last_review_outcome,
            Some(ReviewOutcome::Rejected),
        );
    }

    #[test]
    fn merge_completed_sets_merged_at() {
        let events = vec![
            created("e1", "2026-04-16T00:00:00Z", "asgn-1"),
            Event::MergeCompleted {
                event_id: "e2".to_string(),
                schema_version: CURRENT_SCHEMA_VERSION,
                timestamp: "2026-04-16T00:20:00Z".to_string(),
                assignment_id: "asgn-1".to_string(),
                actor_id: "merger-1".to_string(),
            },
        ];
        let state = project(&events);
        assert_eq!(
            state.assignments["asgn-1"].merged_at.as_deref(),
            Some("2026-04-16T00:20:00Z"),
        );
    }

    #[test]
    fn byte_equal_live_vs_replay() {
        // "Live": apply each event to a running state as it is produced.
        // "Replay": throw all events into `project` in a single batch.
        // Both paths must produce byte-identical serialized WorldState.
        let events = vec![
            created("e1", "2026-04-16T00:00:00Z", "asgn-1"),
            created("e2", "2026-04-16T00:00:01Z", "asgn-2"),
            transitioned(
                "e3",
                "2026-04-16T00:01:00Z",
                "asgn-1",
                AssignmentPhase::Assigned,
                AssignmentPhase::InProgress,
            ),
            Event::HeartbeatReceived {
                event_id: "e4".to_string(),
                schema_version: CURRENT_SCHEMA_VERSION,
                timestamp: "2026-04-16T00:01:30Z".to_string(),
                assignment_id: "asgn-1".to_string(),
                actor_id: "actor-1".to_string(),
            },
            transitioned(
                "e5",
                "2026-04-16T00:02:00Z",
                "asgn-2",
                AssignmentPhase::Assigned,
                AssignmentPhase::InProgress,
            ),
            transitioned(
                "e6",
                "2026-04-16T00:03:00Z",
                "asgn-1",
                AssignmentPhase::InProgress,
                AssignmentPhase::AwaitingReview,
            ),
        ];

        let mut live = WorldState::default();
        let mut seen: HashSet<String> = HashSet::new();
        for e in &events {
            if seen.insert(e.event_id().to_string()) {
                apply_event(&mut live, e);
            }
        }

        let replayed = project(&events);

        let live_bytes = serde_json::to_vec(&live).unwrap();
        let replay_bytes = serde_json::to_vec(&replayed).unwrap();
        assert_eq!(live_bytes, replay_bytes, "live and replayed bytes diverge");
        assert_eq!(live, replayed);
    }

    #[test]
    fn rejected_to_in_repair_increments_repair_cycle_count() {
        // Every Rejected → InRepair edge is counted by the projector so a
        // replayed WorldState ends with the same repair_cycle_count the
        // DB carries. Other phase edges must leave the counter alone.
        let events = vec![
            created("e1", "2026-04-16T00:00:00Z", "asgn-1"),
            transitioned(
                "e2",
                "2026-04-16T00:01:00Z",
                "asgn-1",
                AssignmentPhase::Rejected,
                AssignmentPhase::InRepair,
            ),
            transitioned(
                "e3",
                "2026-04-16T00:02:00Z",
                "asgn-1",
                AssignmentPhase::InRepair,
                AssignmentPhase::AwaitingReview,
            ),
            transitioned(
                "e4",
                "2026-04-16T00:03:00Z",
                "asgn-1",
                AssignmentPhase::Rejected,
                AssignmentPhase::InRepair,
            ),
        ];
        let state = project(&events);

        let view = &state.assignments["asgn-1"];
        assert_eq!(view.repair_cycle_count, 2);
        assert_eq!(view.phase, AssignmentPhase::InRepair);
    }
}
