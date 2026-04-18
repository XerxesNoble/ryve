// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Replay test for the projector.
//!
//! Simulates a workshop that advances assignments through their state
//! machine, records every state change as an `Event` in the outbox, and
//! keeps a "live" `WorldState` updated alongside the DB. At the end, the
//! full event stream is fed back into the pure `project` function and the
//! replayed state is asserted byte-equal to the live state.
//!
//! The "live" path here doesn't use the SQLite layer directly because the
//! transactional writer spark is still a thin wrapper — this test exercises
//! the invariant the projector is responsible for: `project(events)` must
//! equal the step-by-step application of those same events.

use std::collections::BTreeMap;

use data::sparks::projector::{AssignmentView, CURRENT_SCHEMA_VERSION, Event, WorldState, project};
use data::sparks::types::{AssignmentLiveness, AssignmentPhase};

/// A hand-written reducer used by the "live" side. Deliberately mirrors
/// what the transactional writer will emit — if the projector and this
/// reducer drift, byte-equal breaks and the test fails loudly.
fn live_apply(state: &mut WorldState, event: &Event) {
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
            state.assignments.insert(
                assignment_id.clone(),
                AssignmentView {
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
                    last_review_outcome: None,
                    last_review_at: None,
                    last_merge_precondition_failure: None,
                    merged_at: None,
                },
            );
        }
        Event::PhaseTransitioned {
            timestamp,
            assignment_id,
            to_phase,
            ..
        } => {
            let v = state.assignments.get_mut(assignment_id).unwrap();
            v.phase = *to_phase;
            v.event_version += 1;
            v.updated_at = timestamp.clone();
        }
        Event::HeartbeatReceived {
            timestamp,
            assignment_id,
            ..
        } => {
            let v = state.assignments.get_mut(assignment_id).unwrap();
            v.last_heartbeat_at = Some(timestamp.clone());
            v.event_version += 1;
            v.updated_at = timestamp.clone();
        }
        Event::LivenessTransitioned {
            timestamp,
            assignment_id,
            to_liveness,
            ..
        } => {
            let v = state.assignments.get_mut(assignment_id).unwrap();
            v.liveness = *to_liveness;
            v.event_version += 1;
            v.updated_at = timestamp.clone();
        }
        Event::ReviewRequested {
            timestamp,
            assignment_id,
            ..
        } => {
            let v = state.assignments.get_mut(assignment_id).unwrap();
            v.event_version += 1;
            v.updated_at = timestamp.clone();
        }
        Event::ReviewCompleted {
            timestamp,
            assignment_id,
            approved,
            ..
        } => {
            use data::sparks::projector::ReviewOutcome;
            let v = state.assignments.get_mut(assignment_id).unwrap();
            v.last_review_outcome = Some(if *approved {
                ReviewOutcome::Approved
            } else {
                ReviewOutcome::Rejected
            });
            v.last_review_at = Some(timestamp.clone());
            v.event_version += 1;
            v.updated_at = timestamp.clone();
        }
        Event::MergePreconditionFailed {
            timestamp,
            assignment_id,
            reason,
            ..
        } => {
            let v = state.assignments.get_mut(assignment_id).unwrap();
            v.last_merge_precondition_failure = Some(reason.clone());
            v.event_version += 1;
            v.updated_at = timestamp.clone();
        }
        Event::MergeCompleted {
            timestamp,
            assignment_id,
            ..
        } => {
            let v = state.assignments.get_mut(assignment_id).unwrap();
            v.merged_at = Some(timestamp.clone());
            v.event_version += 1;
            v.updated_at = timestamp.clone();
        }
    }
}

/// Simulator for one workshop session. On every command, it updates the
/// live state AND appends an event to the outbox. The two paths are kept
/// in lockstep — the point of the test is that they stay that way even
/// when the replay happens against a fresh, empty WorldState.
struct Workshop {
    live: WorldState,
    outbox: Vec<Event>,
    clock_seconds: u64,
    next_event: u64,
}

impl Workshop {
    fn new() -> Self {
        Self {
            live: WorldState::default(),
            outbox: Vec::new(),
            clock_seconds: 0,
            next_event: 0,
        }
    }

    fn tick(&mut self) -> String {
        // Monotonic, deterministic ISO-8601 timestamps.
        let s = self.clock_seconds;
        self.clock_seconds += 1;
        format!("2026-04-16T00:{:02}:{:02}Z", (s / 60) % 60, s % 60,)
    }

    fn next_id(&mut self) -> String {
        let n = self.next_event;
        self.next_event += 1;
        format!("evt-{n:04}")
    }

    fn emit(&mut self, event: Event) {
        live_apply(&mut self.live, &event);
        self.outbox.push(event);
    }

    fn create(
        &mut self,
        assignment_id: &str,
        spark_id: &str,
        actor_id: &str,
        source_branch: Option<&str>,
        target_branch: Option<&str>,
    ) {
        let event_id = self.next_id();
        let timestamp = self.tick();
        self.emit(Event::AssignmentCreated {
            event_id,
            schema_version: CURRENT_SCHEMA_VERSION,
            timestamp,
            assignment_id: assignment_id.to_string(),
            actor_id: actor_id.to_string(),
            spark_id: spark_id.to_string(),
            initial_phase: AssignmentPhase::Assigned,
            source_branch: source_branch.map(str::to_string),
            target_branch: target_branch.map(str::to_string),
        });
    }

    fn transition(
        &mut self,
        assignment_id: &str,
        actor_id: &str,
        from: AssignmentPhase,
        to: AssignmentPhase,
    ) {
        let event_id = self.next_id();
        let timestamp = self.tick();
        self.emit(Event::PhaseTransitioned {
            event_id,
            schema_version: CURRENT_SCHEMA_VERSION,
            timestamp,
            assignment_id: assignment_id.to_string(),
            actor_id: actor_id.to_string(),
            from_phase: from,
            to_phase: to,
        });
    }

    fn heartbeat(&mut self, assignment_id: &str, actor_id: &str) {
        let event_id = self.next_id();
        let timestamp = self.tick();
        self.emit(Event::HeartbeatReceived {
            event_id,
            schema_version: CURRENT_SCHEMA_VERSION,
            timestamp,
            assignment_id: assignment_id.to_string(),
            actor_id: actor_id.to_string(),
        });
    }

    fn review(&mut self, assignment_id: &str, reviewer: &str, approved: bool) {
        let req_id = self.next_id();
        let req_ts = self.tick();
        self.emit(Event::ReviewRequested {
            event_id: req_id,
            schema_version: CURRENT_SCHEMA_VERSION,
            timestamp: req_ts,
            assignment_id: assignment_id.to_string(),
            actor_id: reviewer.to_string(),
        });

        let done_id = self.next_id();
        let done_ts = self.tick();
        self.emit(Event::ReviewCompleted {
            event_id: done_id,
            schema_version: CURRENT_SCHEMA_VERSION,
            timestamp: done_ts,
            assignment_id: assignment_id.to_string(),
            actor_id: reviewer.to_string(),
            approved,
        });
    }

    fn merge(&mut self, assignment_id: &str, merger: &str) {
        let event_id = self.next_id();
        let timestamp = self.tick();
        self.emit(Event::MergeCompleted {
            event_id,
            schema_version: CURRENT_SCHEMA_VERSION,
            timestamp,
            assignment_id: assignment_id.to_string(),
            actor_id: merger.to_string(),
        });
    }
}

#[test]
fn replay_is_byte_equal_to_live_writes() {
    let mut ws = Workshop::new();

    // Walk assignment A through the happy path.
    ws.create(
        "asgn-A",
        "sp-alpha",
        "hand-1",
        Some("hand/a1"),
        Some("main"),
    );
    ws.transition(
        "asgn-A",
        "hand-1",
        AssignmentPhase::Assigned,
        AssignmentPhase::InProgress,
    );
    ws.heartbeat("asgn-A", "hand-1");
    ws.transition(
        "asgn-A",
        "hand-1",
        AssignmentPhase::InProgress,
        AssignmentPhase::AwaitingReview,
    );
    ws.review("asgn-A", "reviewer-1", true);
    ws.transition(
        "asgn-A",
        "reviewer-1",
        AssignmentPhase::AwaitingReview,
        AssignmentPhase::Approved,
    );
    ws.transition(
        "asgn-A",
        "merger-1",
        AssignmentPhase::Approved,
        AssignmentPhase::ReadyForMerge,
    );
    ws.merge("asgn-A", "merger-1");
    ws.transition(
        "asgn-A",
        "merger-1",
        AssignmentPhase::ReadyForMerge,
        AssignmentPhase::Merged,
    );

    // Assignment B walks through rejection → repair → re-review.
    ws.create("asgn-B", "sp-beta", "hand-2", Some("hand/b1"), Some("main"));
    ws.transition(
        "asgn-B",
        "hand-2",
        AssignmentPhase::Assigned,
        AssignmentPhase::InProgress,
    );
    ws.heartbeat("asgn-B", "hand-2");
    ws.heartbeat("asgn-B", "hand-2");
    ws.transition(
        "asgn-B",
        "hand-2",
        AssignmentPhase::InProgress,
        AssignmentPhase::AwaitingReview,
    );
    ws.review("asgn-B", "reviewer-1", false);
    ws.transition(
        "asgn-B",
        "reviewer-1",
        AssignmentPhase::AwaitingReview,
        AssignmentPhase::Rejected,
    );
    ws.transition(
        "asgn-B",
        "hand-2",
        AssignmentPhase::Rejected,
        AssignmentPhase::InRepair,
    );
    ws.transition(
        "asgn-B",
        "hand-2",
        AssignmentPhase::InRepair,
        AssignmentPhase::AwaitingReview,
    );

    // Assignment C stays in-progress — exercises partial state in replay.
    ws.create("asgn-C", "sp-gamma", "hand-3", None, None);
    ws.transition(
        "asgn-C",
        "hand-3",
        AssignmentPhase::Assigned,
        AssignmentPhase::InProgress,
    );

    // Dump the outbox and rebuild state in a fresh workshop via `project`.
    let events = ws.outbox.clone();
    let replayed = project(&events);

    assert_eq!(ws.live, replayed, "live and replayed states diverge");

    let live_bytes = serde_json::to_vec(&ws.live).expect("serialize live");
    let replay_bytes = serde_json::to_vec(&replayed).expect("serialize replay");
    assert_eq!(
        live_bytes, replay_bytes,
        "byte-equal invariant violated: live and replayed serialize differently"
    );

    // Sanity: every assignment we created shows up in the replay.
    let ids: BTreeMap<_, _> = replayed
        .assignments
        .iter()
        .map(|(k, v)| (k.as_str(), v.phase))
        .collect();
    assert_eq!(ids["asgn-A"], AssignmentPhase::Merged);
    assert_eq!(ids["asgn-B"], AssignmentPhase::AwaitingReview);
    assert_eq!(ids["asgn-C"], AssignmentPhase::InProgress);
}

#[test]
fn replaying_with_duplicates_matches_non_duplicated() {
    let mut ws = Workshop::new();
    ws.create("asgn-A", "sp-alpha", "hand-1", None, None);
    ws.transition(
        "asgn-A",
        "hand-1",
        AssignmentPhase::Assigned,
        AssignmentPhase::InProgress,
    );
    ws.heartbeat("asgn-A", "hand-1");

    let clean = project(&ws.outbox);

    // Replay the exact same stream a second time appended to itself.
    let doubled: Vec<Event> = ws.outbox.iter().chain(ws.outbox.iter()).cloned().collect();
    let replayed = project(&doubled);

    assert_eq!(clean, replayed, "duplicates must be no-ops");
}

#[test]
fn replay_order_within_assignment_matters_but_across_is_stable() {
    // Interleaved event order across different assignments must not affect
    // the final state as long as per-assignment ordering is preserved.
    let mut original = Workshop::new();
    original.create("asgn-A", "sp-1", "h1", None, None);
    original.create("asgn-B", "sp-2", "h2", None, None);
    original.transition(
        "asgn-A",
        "h1",
        AssignmentPhase::Assigned,
        AssignmentPhase::InProgress,
    );
    original.transition(
        "asgn-B",
        "h2",
        AssignmentPhase::Assigned,
        AssignmentPhase::InProgress,
    );

    // Build the "same" events but with per-assignment streams concatenated
    // rather than interleaved. Timestamps and event_ids are preserved so
    // serialized bytes still match.
    let mut a_events = Vec::new();
    let mut b_events = Vec::new();
    for e in &original.outbox {
        if e.assignment_id() == "asgn-A" {
            a_events.push(e.clone());
        } else {
            b_events.push(e.clone());
        }
    }
    let mut reordered = a_events;
    reordered.extend(b_events);

    let replay_a = project(&original.outbox);
    let replay_b = project(&reordered);
    assert_eq!(replay_a, replay_b);
}
