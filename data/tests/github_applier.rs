// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration test for the GitHub mirror applier.
//!
//! Feeds a fixture stream of canonical events through
//! [`data::github::apply`] and asserts:
//!
//! 1. Final Assignment rows match the expected phase + artifact state.
//! 2. The `event_outbox` rows emitted match the expected sequence —
//!    exact event types, ordering, and payload tags.
//!
//! The stream exercises every applier path: dedup short-circuit,
//! PR-opened artifact population, legal transitions through the
//! validator (review approved, PR merged), illegal transitions caught
//! by the validator (CI failure on an already-approved PR), and
//! orphaned events with no matching Assignment. Covers the full
//! acceptance criteria of spark [sp-73e42cac].

use data::github::{
    AppliedOutcome, CanonicalGitHubEvent, EVT_ARTIFACT_RECORDED, EVT_ILLEGAL_TRANSITION_WARNING,
    EVT_ORPHAN_EVENT_WARNING, EVT_PHASE_TRANSITIONED, GithubEventsSeenRepo, apply,
};
use data::sparks::error::TransitionError;

/// One step in the fixture stream. The tuple shape keeps the scenario
/// readable while still carrying everything the applier needs.
struct Step {
    github_event_id: &'static str,
    event: CanonicalGitHubEvent,
}

async fn seed_workgraph(pool: &sqlx::SqlitePool) {
    // A workshop + one spark so the foreign keys hold.
    sqlx::query(
        "INSERT INTO sparks (id, title, description, status, priority, spark_type, \
         workshop_id, metadata, created_at, updated_at) \
         VALUES ('sp-hand', 'hand spark', '', 'open', 2, 'task', 'ws-test', '{}', \
                 '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z'), \
                ('sp-epic', 'epic spark', '', 'open', 2, 'epic', 'ws-test', '{}', \
                 '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed sparks");

    // Assignment A: Hand work, already submitted for review. PR has not
    // been mirrored yet — PrOpened must fill the artifact columns.
    sqlx::query(
        "INSERT INTO assignments \
         (assignment_id, spark_id, actor_id, session_id, status, role, event_version, \
          assignment_phase, source_branch, target_branch, assigned_at, created_at, updated_at) \
         VALUES ('asgn-hand', 'sp-hand', 'actor-alice', 'sess-alice', 'active', 'owner', 3, \
                 'awaiting_review', 'hand/alice', 'main', \
                 '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed hand assignment");

    // Assignment B: Epic MergeHand, artifact already recorded (PR opened
    // in a prior ingest pass), currently ReadyForMerge. PrMerged should
    // advance it to Merged.
    sqlx::query(
        "INSERT INTO assignments \
         (assignment_id, spark_id, actor_id, session_id, status, role, event_version, \
          assignment_phase, source_branch, target_branch, \
          github_artifact_branch, github_artifact_pr_number, \
          assigned_at, created_at, updated_at) \
         VALUES ('asgn-merge', 'sp-epic', 'actor-merger', 'sess-merger', 'active', 'merger', 7, \
                 'ready_for_merge', 'crew/cr-1', 'main', \
                 'crew/cr-1', 200, \
                 '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z', '2026-04-18T00:00:00Z')",
    )
    .execute(pool)
    .await
    .expect("seed merge assignment");
}

fn fixture_stream() -> Vec<Step> {
    vec![
        // 1. PR opened for Hand → artifact recorded on asgn-hand.
        Step {
            github_event_id: "gh-001",
            event: CanonicalGitHubEvent::PrOpened {
                pr_number: 101,
                head_branch: "hand/alice".into(),
            },
        },
        // 2. Same delivery id replayed → Duplicate, no writes.
        Step {
            github_event_id: "gh-001",
            event: CanonicalGitHubEvent::PrOpened {
                pr_number: 101,
                head_branch: "hand/alice".into(),
            },
        },
        // 3. PR metadata edited → Ignored, marked seen only.
        Step {
            github_event_id: "gh-002",
            event: CanonicalGitHubEvent::PrUpdated {
                pr_number: 101,
                head_branch: "hand/alice".into(),
            },
        },
        // 4. Review approved → AwaitingReview → Approved via validator.
        Step {
            github_event_id: "gh-003",
            event: CanonicalGitHubEvent::ReviewApproved {
                pr_number: 101,
                reviewer: "bob".into(),
            },
        },
        // 5. CI failure after approval → Approved → Rejected is illegal;
        // applier emits warning + returns Err.
        Step {
            github_event_id: "gh-004",
            event: CanonicalGitHubEvent::CheckRunStatus {
                pr_number: 101,
                check_name: "ci/build".into(),
                status: "failure".into(),
            },
        },
        // 6. PR merged on the Epic's MergeHand → ReadyForMerge → Merged.
        Step {
            github_event_id: "gh-005",
            event: CanonicalGitHubEvent::PrMerged {
                pr_number: 200,
                merge_commit_sha: "deadbeef".into(),
            },
        },
        // 7. Orphan: PR opened on an unknown branch → NoAssignment warning.
        Step {
            github_event_id: "gh-006",
            event: CanonicalGitHubEvent::PrOpened {
                pr_number: 999,
                head_branch: "hand/unknown".into(),
            },
        },
    ]
}

/// Apply one event in its own transaction, committing even on validator
/// rejection so warning rows and the seen-marker persist (matches the
/// applier's documented contract).
async fn apply_step(
    pool: &sqlx::SqlitePool,
    step: &Step,
) -> Result<AppliedOutcome, data::github::ApplyError> {
    let seen = GithubEventsSeenRepo::new();
    let mut tx = pool.begin().await.expect("begin");
    let result = apply(&mut tx, step.github_event_id, &step.event, &seen).await;
    tx.commit()
        .await
        .expect("commit (warnings + seen must persist)");
    result
}

#[sqlx::test]
async fn applier_drives_full_lifecycle(pool: sqlx::SqlitePool) {
    seed_workgraph(&pool).await;

    let mut outcomes = Vec::new();
    for step in fixture_stream() {
        outcomes.push(apply_step(&pool, &step).await);
    }

    // ── outcome-by-outcome assertions ───────────────────────────────

    // 1. PrOpened on Hand → ArtifactRecorded.
    let asgn_hand_id = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM assignments WHERE assignment_id = 'asgn-hand'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        matches!(
            outcomes[0].as_ref().unwrap(),
            AppliedOutcome::ArtifactRecorded { assignment_id, pr_number: 101 } if *assignment_id == asgn_hand_id,
        ),
        "step 1 outcome: {:?}",
        outcomes[0],
    );

    // 2. Duplicate short-circuits.
    assert!(
        matches!(outcomes[1].as_ref().unwrap(), AppliedOutcome::Duplicate),
        "step 2 outcome: {:?}",
        outcomes[1],
    );

    // 3. PrUpdated is non-state-changing.
    assert!(
        matches!(outcomes[2].as_ref().unwrap(), AppliedOutcome::Ignored),
        "step 3 outcome: {:?}",
        outcomes[2],
    );

    // 4. ReviewApproved → AwaitingReview → Approved.
    assert!(
        matches!(
            outcomes[3].as_ref().unwrap(),
            AppliedOutcome::Transitioned {
                from: data::sparks::types::AssignmentPhase::AwaitingReview,
                to: data::sparks::types::AssignmentPhase::Approved,
                ..
            },
        ),
        "step 4 outcome: {:?}",
        outcomes[3],
    );

    // 5. CheckRunStatus on Approved → illegal transition Err.
    let err = outcomes[4].as_ref().unwrap_err();
    assert!(
        matches!(
            err,
            data::github::ApplyError::Transition(TransitionError::IllegalTransition { .. }),
        ),
        "step 5 should be IllegalTransition, got {err:?}",
    );

    // 6. PrMerged → ReadyForMerge → Merged on the MergeHand.
    let asgn_merge_id = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM assignments WHERE assignment_id = 'asgn-merge'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        matches!(
            outcomes[5].as_ref().unwrap(),
            AppliedOutcome::Transitioned {
                assignment_id,
                from: data::sparks::types::AssignmentPhase::ReadyForMerge,
                to: data::sparks::types::AssignmentPhase::Merged,
            } if *assignment_id == asgn_merge_id,
        ),
        "step 6 outcome: {:?}",
        outcomes[5],
    );

    // 7. Orphan branch → NoAssignment warning.
    assert!(
        matches!(
            outcomes[6].as_ref().unwrap(),
            AppliedOutcome::NoAssignment { pr_number: 999 },
        ),
        "step 7 outcome: {:?}",
        outcomes[6],
    );

    // ── Final Assignment state ──────────────────────────────────────

    let (phase, branch, pr, event_version): (String, Option<String>, Option<i64>, i64) =
        sqlx::query_as(
            "SELECT assignment_phase, github_artifact_branch, github_artifact_pr_number, \
             event_version \
             FROM assignments WHERE assignment_id = 'asgn-hand'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(phase, "approved", "hand assignment must end approved");
    assert_eq!(branch.as_deref(), Some("hand/alice"));
    assert_eq!(pr, Some(101));
    assert_eq!(
        event_version, 4,
        "event_version should bump once per successful transition"
    );

    let (phase, pr): (String, Option<i64>) = sqlx::query_as(
        "SELECT assignment_phase, github_artifact_pr_number FROM assignments \
         WHERE assignment_id = 'asgn-merge'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(phase, "merged", "merge assignment must end merged");
    assert_eq!(pr, Some(200));

    // ── Exact event_outbox sequence ────────────────────────────────

    let outbox: Vec<(String, String)> = sqlx::query_as(
        "SELECT event_type, assignment_id FROM event_outbox ORDER BY timestamp ASC, event_id ASC",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    // Expected: exactly five rows — one per durable effect, none for
    // Duplicate/Ignored.
    let expected: Vec<(&str, &str)> = vec![
        (EVT_ARTIFACT_RECORDED, "asgn-hand"),
        (EVT_PHASE_TRANSITIONED, "asgn-hand"),
        (EVT_ILLEGAL_TRANSITION_WARNING, "asgn-hand"),
        (EVT_PHASE_TRANSITIONED, "asgn-merge"),
        (EVT_ORPHAN_EVENT_WARNING, "github-orphan"),
    ];
    let actual: Vec<(&str, &str)> = outbox
        .iter()
        .map(|(t, a)| (t.as_str(), a.as_str()))
        .collect();
    assert_eq!(actual, expected, "outbox sequence mismatch");

    // ── Dedup log ──────────────────────────────────────────────────

    let seen_ids: Vec<String> = sqlx::query_scalar(
        "SELECT github_event_id FROM github_events_seen ORDER BY github_event_id ASC",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        seen_ids,
        vec![
            "gh-001".to_string(),
            "gh-002".to_string(),
            "gh-003".to_string(),
            "gh-004".to_string(),
            "gh-005".to_string(),
            "gh-006".to_string(),
        ],
        "every delivery id must be recorded exactly once",
    );
}

#[sqlx::test]
async fn applier_is_idempotent_across_repeated_streams(pool: sqlx::SqlitePool) {
    // Feeding the same stream twice must produce the same final state
    // as feeding it once — the dedup log short-circuits every event on
    // the second pass.
    seed_workgraph(&pool).await;

    for _ in 0..2 {
        for step in fixture_stream() {
            let _ = apply_step(&pool, &step).await;
        }
    }

    let outbox_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        outbox_count, 5,
        "re-running the stream must not produce extra outbox rows"
    );

    let phase: String = sqlx::query_scalar(
        "SELECT assignment_phase FROM assignments WHERE assignment_id = 'asgn-hand'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(phase, "approved");
}
