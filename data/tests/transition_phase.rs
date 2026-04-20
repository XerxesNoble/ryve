use data::sparks::error::TransitionError;
use data::sparks::heartbeat_watchdog::{
    LIVENESS_TRANSITIONED_EVENT_TYPE, LivenessTransitionedPayload,
};
use data::sparks::transition;
use data::sparks::types::{AssignmentLiveness, AssignmentPhase, TransitionActorRole};

async fn seed_assignment(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query(
        "INSERT INTO agent_sessions (id, workshop_id, agent_name, agent_command, status, started_at)
         VALUES ('sess-trans-01', 'ws-test', 'claude', 'claude', 'active', '2026-04-09T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed agent_session");

    sqlx::query(
        "INSERT INTO sparks (id, title, description, status, priority, spark_type, workshop_id, metadata, created_at, updated_at)
         VALUES ('sp-trans-1', 'test spark', '', 'open', 2, 'task', 'ws-test', '{}', '2026-04-09T00:00:00Z', '2026-04-09T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed spark");

    sqlx::query(
        "INSERT INTO assignments (assignment_id, spark_id, actor_id, session_id, status, role, event_version, assigned_at, assignment_phase, created_at, updated_at)
         VALUES ('asgn-trans-01', 'sp-trans-1', 'sess-trans-01', 'sess-trans-01', 'active', 'owner', 0, '2026-04-09T00:00:00Z', 'assigned', '2026-04-09T00:00:00Z', '2026-04-09T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed assignment");

    sqlx::query_scalar::<_, i64>("SELECT id FROM assignments WHERE spark_id = 'sp-trans-1'")
        .fetch_one(pool)
        .await
        .expect("get assignment id")
}

#[sqlx::test]
async fn transition_phase_assigned_to_in_progress(pool: sqlx::SqlitePool) {
    let assignment_id = seed_assignment(&pool).await;

    let updated = transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "actor-hand-1",
        TransitionActorRole::Hand,
        AssignmentPhase::InProgress,
        AssignmentPhase::Assigned,
        1,
    )
    .await
    .expect("transition should succeed");

    assert_eq!(
        updated.assignment_phase.as_deref(),
        Some("in_progress"),
        "assignment_phase should be updated to in_progress"
    );
    assert!(
        updated.phase_changed_at.is_some(),
        "phase_changed_at should be set"
    );
    assert_eq!(
        updated.phase_changed_by.as_deref(),
        Some("actor-hand-1"),
        "phase_changed_by should record the actor"
    );
    assert_eq!(
        updated.phase_actor_role.as_deref(),
        Some("hand"),
        "phase_actor_role should record the role"
    );
    assert!(
        updated.phase_event_id.is_some(),
        "phase_event_id should reference the appended event"
    );
}

#[sqlx::test]
async fn transition_phase_appends_event_atomically(pool: sqlx::SqlitePool) {
    let assignment_id = seed_assignment(&pool).await;

    let updated = transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "actor-hand-1",
        TransitionActorRole::Hand,
        AssignmentPhase::InProgress,
        AssignmentPhase::Assigned,
        1,
    )
    .await
    .expect("transition should succeed");

    let event_id = updated.phase_event_id.expect("event_id should be set");
    let event = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        "SELECT field_name, old_value, new_value FROM events WHERE id = ?",
    )
    .bind(event_id)
    .fetch_one(&pool)
    .await
    .expect("event should exist");

    assert_eq!(event.0, "assignment_phase");
    assert_eq!(event.1.as_deref(), Some("assigned"));
    assert_eq!(event.2.as_deref(), Some("in_progress"));
}

#[sqlx::test]
async fn transition_phase_full_happy_path(pool: sqlx::SqlitePool) {
    let assignment_id = seed_assignment(&pool).await;

    let steps: &[(AssignmentPhase, AssignmentPhase, TransitionActorRole, &str)] = &[
        (
            AssignmentPhase::Assigned,
            AssignmentPhase::InProgress,
            TransitionActorRole::Hand,
            "hand-1",
        ),
        (
            AssignmentPhase::InProgress,
            AssignmentPhase::AwaitingReview,
            TransitionActorRole::Hand,
            "hand-1",
        ),
        (
            AssignmentPhase::AwaitingReview,
            AssignmentPhase::Approved,
            TransitionActorRole::ReviewerHand,
            "reviewer-1",
        ),
        (
            AssignmentPhase::Approved,
            AssignmentPhase::ReadyForMerge,
            TransitionActorRole::MergeHand,
            "merger-1",
        ),
        (
            AssignmentPhase::ReadyForMerge,
            AssignmentPhase::Merged,
            TransitionActorRole::MergeHand,
            "merger-1",
        ),
    ];

    for (i, (from, to, role, actor)) in steps.iter().enumerate() {
        let updated = transition::transition_assignment_phase(
            &pool,
            assignment_id,
            actor,
            *role,
            *to,
            *from,
            (i + 1) as i64,
        )
        .await
        .unwrap_or_else(|e| panic!("step {i}: {from:?} → {to:?} failed: {e}"));

        assert_eq!(
            updated.assignment_phase.as_deref(),
            Some(to.as_str()),
            "step {i}: phase should be {to:?}"
        );
        assert!(
            updated.phase_event_id.is_some(),
            "step {i}: event must be recorded"
        );
    }

    let events = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM events WHERE spark_id = 'sp-trans-1' AND field_name = 'assignment_phase'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 5, "all 5 transitions should produce events");
}

/// Author is recorded as `assignments.actor_id = 'sess-trans-01'` by
/// `seed_assignment`. A ReviewerHand whose `actor_id` matches must be
/// refused when driving AwaitingReview → Approved, even though the role
/// check alone would pass. This is the transition-time half of the
/// reviewer-identity invariant; the selection-time half lives in the
/// pure `ensure_reviewer_not_author` unit tests.
#[sqlx::test]
async fn reviewer_cannot_approve_own_assignment(pool: sqlx::SqlitePool) {
    let assignment_id = seed_assignment(&pool).await;

    transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::Hand,
        AssignmentPhase::InProgress,
        AssignmentPhase::Assigned,
        1,
    )
    .await
    .expect("author starts work");

    transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::Hand,
        AssignmentPhase::AwaitingReview,
        AssignmentPhase::InProgress,
        2,
    )
    .await
    .expect("author submits for review");

    // Same actor attempts to review their own work.
    let err = transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::ReviewerHand,
        AssignmentPhase::Approved,
        AssignmentPhase::AwaitingReview,
        3,
    )
    .await
    .expect_err("reviewer == author must be refused");

    match err {
        TransitionError::ReviewerIsAuthor { reviewer_actor_id } => {
            assert_eq!(reviewer_actor_id, "sess-trans-01");
        }
        other => panic!("expected ReviewerIsAuthor, got {other:?}"),
    }

    // Phase must remain AwaitingReview — no state mutation on rejection.
    let phase =
        sqlx::query_scalar::<_, String>("SELECT assignment_phase FROM assignments WHERE id = ?")
            .bind(assignment_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(phase, "awaiting_review");
}

#[sqlx::test]
async fn reviewer_distinct_from_author_can_approve(pool: sqlx::SqlitePool) {
    let assignment_id = seed_assignment(&pool).await;

    transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::Hand,
        AssignmentPhase::InProgress,
        AssignmentPhase::Assigned,
        1,
    )
    .await
    .unwrap();
    transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::Hand,
        AssignmentPhase::AwaitingReview,
        AssignmentPhase::InProgress,
        2,
    )
    .await
    .unwrap();

    let updated = transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "actor-reviewer-9",
        TransitionActorRole::ReviewerHand,
        AssignmentPhase::Approved,
        AssignmentPhase::AwaitingReview,
        3,
    )
    .await
    .expect("distinct reviewer must succeed");

    assert_eq!(updated.assignment_phase.as_deref(), Some("approved"));
    assert_eq!(
        updated.phase_changed_by.as_deref(),
        Some("actor-reviewer-9")
    );
    assert_eq!(updated.phase_actor_role.as_deref(), Some("reviewer_hand"));
}

// ── Repair-cycle counter + stuck escalation ────────────

/// Walk a freshly-seeded assignment through
/// Assigned → InProgress → AwaitingReview → Rejected and stop there so the
/// repair-cycle tests can drive the Rejected → InRepair edge in isolation.
/// Returns the assignment's integer primary key and the reviewer actor id
/// so callers can keep pushing the state machine forward.
async fn walk_to_rejected(pool: &sqlx::SqlitePool) -> (i64, &'static str) {
    let assignment_id = seed_assignment(pool).await;
    let reviewer = "actor-reviewer-42";

    transition::transition_assignment_phase(
        pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::Hand,
        AssignmentPhase::InProgress,
        AssignmentPhase::Assigned,
        1,
    )
    .await
    .expect("start work");
    transition::transition_assignment_phase(
        pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::Hand,
        AssignmentPhase::AwaitingReview,
        AssignmentPhase::InProgress,
        2,
    )
    .await
    .expect("submit for review");
    transition::transition_assignment_phase(
        pool,
        assignment_id,
        reviewer,
        TransitionActorRole::ReviewerHand,
        AssignmentPhase::Rejected,
        AssignmentPhase::AwaitingReview,
        3,
    )
    .await
    .expect("reviewer rejects");

    (assignment_id, reviewer)
}

async fn read_repair_count(pool: &sqlx::SqlitePool, assignment_id: i64) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT repair_cycle_count FROM assignments WHERE id = ?")
        .bind(assignment_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn read_liveness(pool: &sqlx::SqlitePool, assignment_id: i64) -> AssignmentLiveness {
    let s: String = sqlx::query_scalar("SELECT liveness FROM assignments WHERE id = ?")
        .bind(assignment_id)
        .fetch_one(pool)
        .await
        .unwrap();
    AssignmentLiveness::from_str(&s).unwrap_or_else(|| panic!("unknown liveness {s:?}"))
}

/// A single Rejected → InRepair transition increments repair_cycle_count
/// from 0 to 1 and, under the default limit of 3, does not escalate
/// liveness. No canonical Stuck event should hit the outbox yet.
#[sqlx::test]
async fn rejected_to_in_repair_increments_repair_cycle_counter(pool: sqlx::SqlitePool) {
    let (assignment_id, _reviewer) = walk_to_rejected(&pool).await;
    assert_eq!(read_repair_count(&pool, assignment_id).await, 0);

    transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::Hand,
        AssignmentPhase::InRepair,
        AssignmentPhase::Rejected,
        4,
    )
    .await
    .expect("enter repair");

    assert_eq!(read_repair_count(&pool, assignment_id).await, 1);
    assert_eq!(
        read_liveness(&pool, assignment_id).await,
        AssignmentLiveness::Healthy,
        "under the repair_cycle_limit, liveness must remain Healthy"
    );

    let outbox_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox \
         WHERE assignment_id = (SELECT assignment_id FROM assignments WHERE id = ?) \
         AND event_type = ?",
    )
    .bind(assignment_id)
    .bind(LIVENESS_TRANSITIONED_EVENT_TYPE)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        outbox_count, 0,
        "no LivenessTransitioned event should be emitted below the limit"
    );
}

/// Exceeding workshop.repair_cycle_limit during a Rejected → InRepair
/// transition must escalate the assignment to Stuck and emit the
/// canonical LivenessTransitioned outbox row so downstream subscribers
/// (IRC, projector) see the same event the watchdog writes.
#[sqlx::test]
async fn repair_cycle_limit_escalates_to_stuck_with_canonical_event(pool: sqlx::SqlitePool) {
    let (assignment_id, reviewer) = walk_to_rejected(&pool).await;

    // Default repair_cycle_limit is 3. Walk through Rejected → InRepair
    // → AwaitingReview → Rejected three times so `repair_cycle_count`
    // reaches 3 while liveness stays Healthy — the escalation fires
    // only on the fourth re-entry into InRepair.
    let mut version: i64 = 4;
    for _ in 0..3 {
        transition::transition_assignment_phase(
            &pool,
            assignment_id,
            "sess-trans-01",
            TransitionActorRole::Hand,
            AssignmentPhase::InRepair,
            AssignmentPhase::Rejected,
            version,
        )
        .await
        .expect("enter repair");
        version += 1;
        transition::transition_assignment_phase(
            &pool,
            assignment_id,
            "sess-trans-01",
            TransitionActorRole::Hand,
            AssignmentPhase::AwaitingReview,
            AssignmentPhase::InRepair,
            version,
        )
        .await
        .expect("resubmit for review");
        version += 1;
        transition::transition_assignment_phase(
            &pool,
            assignment_id,
            reviewer,
            TransitionActorRole::ReviewerHand,
            AssignmentPhase::Rejected,
            AssignmentPhase::AwaitingReview,
            version,
        )
        .await
        .expect("reviewer rejects again");
        version += 1;
    }
    assert_eq!(read_repair_count(&pool, assignment_id).await, 3);
    assert_eq!(
        read_liveness(&pool, assignment_id).await,
        AssignmentLiveness::Healthy,
        "reaching the limit exactly must not escalate — only exceeding it"
    );

    // The fourth Rejected → InRepair pushes repair_cycle_count to 4,
    // which is > 3 and therefore triggers the Stuck escalation.
    transition::transition_assignment_phase(
        &pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::Hand,
        AssignmentPhase::InRepair,
        AssignmentPhase::Rejected,
        version,
    )
    .await
    .expect("enter repair, exceeding limit");

    assert_eq!(read_repair_count(&pool, assignment_id).await, 4);
    assert_eq!(
        read_liveness(&pool, assignment_id).await,
        AssignmentLiveness::Stuck,
        "exceeding repair_cycle_limit must escalate liveness to Stuck"
    );

    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT event_type, payload, actor_id FROM event_outbox \
         WHERE assignment_id = (SELECT assignment_id FROM assignments WHERE id = ?) \
         ORDER BY timestamp ASC",
    )
    .bind(assignment_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "exactly one canonical LivenessTransitioned row expected, got {rows:?}"
    );
    assert_eq!(rows[0].0, LIVENESS_TRANSITIONED_EVENT_TYPE);
    assert_eq!(
        rows[0].2,
        transition::REPAIR_CYCLE_ESCALATION_ACTOR,
        "actor_id distinguishes the repair-cycle escalation from watchdog traffic"
    );

    let payload: LivenessTransitionedPayload = serde_json::from_str(&rows[0].1).unwrap();
    assert_eq!(payload.to_liveness, AssignmentLiveness::Stuck);
    assert_eq!(payload.spark_id, "sp-trans-1");
    assert_eq!(
        payload.age_secs, 4,
        "payload surfaces the repair_cycle_count that tripped the escalation"
    );
}

/// Exceeding a caller-supplied repair_cycle_limit works identically to
/// the default path — verifies `transition_assignment_phase_with_limit`
/// honours workshop.repair_cycle_limit.
#[sqlx::test]
async fn custom_repair_cycle_limit_escalates_at_boundary(pool: sqlx::SqlitePool) {
    let (assignment_id, _reviewer) = walk_to_rejected(&pool).await;

    // Limit = 0 means the very first Rejected → InRepair (count becomes
    // 1) exceeds the limit, so escalation must fire immediately.
    transition::transition_assignment_phase_with_limit(
        &pool,
        transition::PhaseTransitionArgs {
            assignment_id,
            actor_id: "sess-trans-01",
            actor_role: TransitionActorRole::Hand,
            target_phase: AssignmentPhase::InRepair,
            expected_previous_phase: AssignmentPhase::Rejected,
            event_version: 4,
            repair_cycle_limit: 0,
        },
    )
    .await
    .expect("custom-limit transition");

    assert_eq!(read_repair_count(&pool, assignment_id).await, 1);
    assert_eq!(
        read_liveness(&pool, assignment_id).await,
        AssignmentLiveness::Stuck
    );
}

/// A Hand calling the direct liveness-to-Stuck API must be rejected with
/// `Unauthorized` — Stuck is only emittable by watchdog (its own path)
/// or by Head/Director overrides.
#[sqlx::test]
async fn hand_cannot_self_transition_to_stuck(pool: sqlx::SqlitePool) {
    let assignment_id = seed_assignment(&pool).await;

    let err = transition::transition_liveness_to_stuck(
        &pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::Hand,
    )
    .await
    .expect_err("Hand must not be able to self-transition to Stuck");

    match err {
        TransitionError::Unauthorized { role, to, .. } => {
            assert_eq!(role, "hand");
            assert_eq!(to, "stuck");
        }
        other => panic!("expected Unauthorized, got {other:?}"),
    }

    assert_eq!(
        read_liveness(&pool, assignment_id).await,
        AssignmentLiveness::Healthy,
        "liveness must not mutate after a rejected Hand request"
    );

    let outbox_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox \
         WHERE assignment_id = (SELECT assignment_id FROM assignments WHERE id = ?)",
    )
    .bind(assignment_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(outbox_count, 0, "rejected request must not emit outbox row");
}

// ── Conflict handoff (Approved → Rejected) [sp-086a4432] ───────

/// Walk the assignment into `Approved` so the conflict-handoff tests
/// can drive the `Approved → Rejected` edge in isolation. Returns the
/// assignment's integer primary key and the final `event_version` so
/// callers know what to pass next.
async fn walk_to_approved(pool: &sqlx::SqlitePool) -> (i64, i64) {
    let assignment_id = seed_assignment(pool).await;
    transition::transition_assignment_phase(
        pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::Hand,
        AssignmentPhase::InProgress,
        AssignmentPhase::Assigned,
        1,
    )
    .await
    .expect("start work");
    transition::transition_assignment_phase(
        pool,
        assignment_id,
        "sess-trans-01",
        TransitionActorRole::Hand,
        AssignmentPhase::AwaitingReview,
        AssignmentPhase::InProgress,
        2,
    )
    .await
    .expect("submit for review");
    transition::transition_assignment_phase(
        pool,
        assignment_id,
        "actor-reviewer-42",
        TransitionActorRole::ReviewerHand,
        AssignmentPhase::Approved,
        AssignmentPhase::AwaitingReview,
        3,
    )
    .await
    .expect("reviewer approves");
    (assignment_id, 3)
}

#[sqlx::test]
async fn merge_hand_can_reject_approved_for_conflict(pool: sqlx::SqlitePool) {
    let (assignment_id, version) = walk_to_approved(&pool).await;

    let updated = transition::reject_approved_for_conflict(
        &pool,
        assignment_id,
        "merge_hand_preflight",
        version + 1,
    )
    .await
    .expect("merge_hand conflict handoff must succeed");

    assert_eq!(updated.assignment_phase.as_deref(), Some("rejected"));
    assert_eq!(updated.phase_actor_role.as_deref(), Some("merge_hand"));
    assert_eq!(
        updated.phase_changed_by.as_deref(),
        Some("merge_hand_preflight")
    );

    // The emitted phase-change event records reason=conflict so replay
    // + audit consumers can tell a Merge-Hand handoff apart from a
    // Reviewer rejection.
    let event_id = updated.phase_event_id.expect("event id recorded");
    let (old_value, new_value, reason): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT old_value, new_value, reason FROM events WHERE id = ?")
            .bind(event_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(old_value.as_deref(), Some("approved"));
    assert_eq!(new_value.as_deref(), Some("rejected"));
    assert_eq!(reason.as_deref(), Some("conflict"));
}

#[sqlx::test]
async fn hand_cannot_reject_approved_for_conflict(pool: sqlx::SqlitePool) {
    let (assignment_id, version) = walk_to_approved(&pool).await;

    // Using an actor distinct from the assignment author so we don't
    // fall through to ReviewerIsAuthor before the role check fires.
    let err = transition::transition_assignment_phase_with_reason(
        &pool,
        transition::PhaseTransitionWithReasonArgs {
            assignment_id,
            actor_id: "actor-rogue",
            actor_role: TransitionActorRole::Hand,
            target_phase: AssignmentPhase::Rejected,
            expected_previous_phase: AssignmentPhase::Approved,
            event_version: version + 1,
            reason: "conflict",
        },
    )
    .await
    .expect_err("Hand must not drive the handoff");
    match err {
        data::sparks::error::TransitionError::Unauthorized { role, from, to, .. } => {
            assert_eq!(role, "hand");
            assert_eq!(from, "approved");
            assert_eq!(to, "rejected");
        }
        other => panic!("expected Unauthorized, got {other:?}"),
    }

    let phase =
        sqlx::query_scalar::<_, String>("SELECT assignment_phase FROM assignments WHERE id = ?")
            .bind(assignment_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        phase, "approved",
        "refused transition must not mutate state"
    );
}

#[sqlx::test]
async fn approved_to_rejected_requires_conflict_reason(pool: sqlx::SqlitePool) {
    let (assignment_id, version) = walk_to_approved(&pool).await;

    // A MergeHand passing the wrong reason must be refused — the
    // reason=conflict tag is the semantic contract for this edge.
    let err = transition::transition_assignment_phase_with_reason(
        &pool,
        transition::PhaseTransitionWithReasonArgs {
            assignment_id,
            actor_id: "merge_hand_preflight",
            actor_role: TransitionActorRole::MergeHand,
            target_phase: AssignmentPhase::Rejected,
            expected_previous_phase: AssignmentPhase::Approved,
            event_version: version + 1,
            reason: "other-reason",
        },
    )
    .await
    .expect_err("wrong reason must be refused");
    assert!(matches!(
        err,
        data::sparks::error::TransitionError::InvalidReason { .. }
    ));

    // Phase must remain Approved — the transition aborted before any
    // UPDATE landed.
    let phase =
        sqlx::query_scalar::<_, String>("SELECT assignment_phase FROM assignments WHERE id = ?")
            .bind(assignment_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(phase, "approved");
}

/// A Head/Director override is allowed to drive the Stuck transition
/// directly. This covers the recovery/override path named in the parent
/// epic without needing the watchdog or a repair-cycle breach.
#[sqlx::test]
async fn head_override_can_transition_to_stuck(pool: sqlx::SqlitePool) {
    let assignment_id = seed_assignment(&pool).await;

    transition::transition_liveness_to_stuck(
        &pool,
        assignment_id,
        "head-oncall-7",
        TransitionActorRole::Head,
    )
    .await
    .expect("Head override must succeed");

    assert_eq!(
        read_liveness(&pool, assignment_id).await,
        AssignmentLiveness::Stuck
    );

    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT event_type, actor_id FROM event_outbox \
         WHERE assignment_id = (SELECT assignment_id FROM assignments WHERE id = ?)",
    )
    .bind(assignment_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, LIVENESS_TRANSITIONED_EVENT_TYPE);
    assert_eq!(
        rows[0].1, "head-oncall-7",
        "override path stamps the caller's actor_id"
    );
}
