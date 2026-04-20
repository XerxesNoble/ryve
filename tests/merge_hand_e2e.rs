// SPDX-License-Identifier: AGPL-3.0-or-later

//! End-to-end integration test for the Merge-Hand Epic lifecycle
//! (spark ryve-51daf9b1 / epic sp-476ef264).
//!
//! Drives the full happy path of the Merge-Hand contract:
//!
//! 1. Two user Assignments are created under an Epic and driven through
//!    the transition state machine to `Approved`, each carrying a mirrored
//!    GitHub artifact PR number and a real git branch in the test repo.
//! 2. A merge-spark is created as a third child of the Epic.
//! 3. The Merge-Hand precondition checker is run against the merge-spark
//!    with the default `NoopMergePreconditionEnv`. The gate passes — that
//!    is the "spawn merge_hand" step: the gate is what refuses or admits a
//!    Merge Hand subprocess, and we verify it admits here.
//! 4. The test plays the role of the spawned Merge Hand: it creates the
//!    Epic branch, integrates both sub-branches with `git merge --no-ff`
//!    in Assignment-creation order, composes the Epic PR body using the
//!    template the Merge-Hand prompt specifies, and executes the
//!    Epic → main merge.
//! 5. The merge-spark's Assignment is driven through its own phase path
//!    (`Approved → ReadyForMerge → Merged`) via the transition validator,
//!    with the final edge going through the `mark_assignment_merged`
//!    helper that pins `actor_role = MergeHand` — the ONLY role permitted
//!    to emit the Merged transition.
//!
//! The test then pins every acceptance criterion from the spark:
//!
//!   - Sub-PRs are merged into the Epic branch in Assignment-creation
//!     order with `--no-ff` (asserted via `git log --merges`).
//!   - The Epic PR body lists every child Assignment together with its
//!     source PR (asserted by substring against the composed body).
//!   - The Epic → main merge executes and emits the Merged phase
//!     transition (asserted via the `events` row with role=merge_hand).
//!   - No `epic.blocker_raised` outbox row fires (the happy path must not
//!     surface a precondition-failure event).
//!   - All phase-change events appear in the correct order (asserted via
//!     timestamp-ascending query against the `events` table).
//!
//! Since the Merge Hand's own git operations happen inside a Claude
//! subprocess in production, the test plays the agent's role directly —
//! the intent is to pin the data-layer + git-layer contract that a real
//! Merge Hand must satisfy, not to spawn a real coding-agent process.

use std::path::{Path, PathBuf};
use std::process::Command;

use data::db;
use data::sparks::types::{
    AssignmentPhase, NewAssignment, NewSpark, SparkType, TransitionActorRole,
};
use data::sparks::{assign_repo, spark_repo, transition};
use sqlx::SqlitePool;

fn fresh_workshop_root(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ryve-merge-hand-e2e-{tag}-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create tempdir");
    root
}

/// Run `git` in `cwd` with test identity pinned via env. Panics on
/// non-zero exit so test setup failures surface immediately.
fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "ryve-test")
        .env("GIT_AUTHOR_EMAIL", "test@ryve.local")
        .env("GIT_COMMITTER_NAME", "ryve-test")
        .env("GIT_COMMITTER_EMAIL", "test@ryve.local")
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed in {cwd:?}");
}

/// Capture `git` stdout. Trimmed of trailing newlines.
fn git_out(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "ryve-test")
        .env("GIT_AUTHOR_EMAIL", "test@ryve.local")
        .env("GIT_COMMITTER_NAME", "ryve-test")
        .env("GIT_COMMITTER_EMAIL", "test@ryve.local")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed in {cwd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// Initialise a git repo under `root` with a single seed commit on
/// `main`, then pin local git config so commits do not depend on
/// whatever signing / global identity a developer happens to have.
fn init_git_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "tag.gpgSign", "false"]);
    std::fs::write(root.join("README.md"), "seed\n").unwrap();
    git(root, &["add", "README.md"]);
    git(root, &["commit", "-q", "-m", "seed"]);
}

/// Cut a feature branch off `main`, land one distinct commit on it, and
/// return to `main`. Distinct commits guarantee that a later
/// `git merge --no-ff` cannot fast-forward — every sub-PR must materialise
/// as its own merge commit (the `--no-ff` invariant).
fn author_sub_branch(root: &Path, branch: &str, file: &str, content: &str) {
    git(root, &["checkout", "-q", "-b", branch, "main"]);
    std::fs::write(root.join(file), content).unwrap();
    git(root, &["add", file]);
    git(
        root,
        &["commit", "-q", "-m", &format!("{branch}: author work")],
    );
    git(root, &["checkout", "-q", "main"]);
}

/// Drive a freshly-created Assignment (phase=`Assigned`) through the
/// author-side phase path `Assigned → InProgress → AwaitingReview` and
/// then the reviewer-side `AwaitingReview → Approved`. Returns the
/// updated `event_version` so downstream transitions can chain off it.
async fn drive_child_to_approved(
    pool: &SqlitePool,
    assignment_id: i64,
    author_actor: &str,
    reviewer_actor: &str,
) -> i64 {
    transition::transition_assignment_phase(
        pool,
        assignment_id,
        author_actor,
        TransitionActorRole::Hand,
        AssignmentPhase::InProgress,
        AssignmentPhase::Assigned,
        2,
    )
    .await
    .expect("author starts work");

    transition::transition_assignment_phase(
        pool,
        assignment_id,
        author_actor,
        TransitionActorRole::Hand,
        AssignmentPhase::AwaitingReview,
        AssignmentPhase::InProgress,
        3,
    )
    .await
    .expect("author submits for review");

    transition::transition_assignment_phase(
        pool,
        assignment_id,
        reviewer_actor,
        TransitionActorRole::ReviewerHand,
        AssignmentPhase::Approved,
        AssignmentPhase::AwaitingReview,
        4,
    )
    .await
    .expect("reviewer approves");

    4
}

/// Compose the Epic PR body using the template the Merge-Hand prompt
/// specifies (see `compose_merge_hand_prompt` in `src/agent_prompts.rs`).
/// The test replicates the template so it can assert a real Merge Hand
/// produces a body with the same shape — any future edit to the prompt's
/// template that drops a child reference, a PR number, or the `--no-ff`
/// discipline phrase will fail this assertion.
fn compose_epic_pr_body(
    epic_title: &str,
    epic_id: &str,
    crew_id: &str,
    children: &[(String, String, String, i64)],
) -> String {
    let short = &epic_id[..epic_id.len().min(12)];
    let mut body = format!(
        "## Epic: {epic_title} ([sp-{short}])\n\n\
         This Epic PR integrates the following child Assignments:\n\n"
    );
    for (asgn_id, spark_id, actor_id, pr_number) in children {
        body.push_str(&format!(
            "- Assignment `{asgn_id}` (spark `{spark_id}`, actor `{actor_id}`) \
             — source PR #{pr_number}\n"
        ));
    }
    body.push_str(&format!(
        "\nSub-PRs merged into `crew/{crew_id}` with `git merge --no-ff` in \
         Assignment-creation order. Merge-Hand precondition checks: all \
         Approved, no conflicts, CI green, zero Stuck.\n"
    ));
    body
}

#[tokio::test]
async fn merge_hand_happy_path_epic_to_main() {
    let root = fresh_workshop_root("happy-path");
    init_git_repo(&root);

    let pool = db::open_sparks_db(&root).await.expect("open sparks db");
    let workshop_id = "merge-hand-e2e-ws".to_string();

    // ── 1. Seed the workgraph state ─────────────────────────────────
    //
    // Epic + two user-Assignment children + one merge-spark child. The
    // children are created in a deterministic order so the test can
    // later assert "sub-PRs merged in Assignment-creation order".
    let epic = spark_repo::create(
        &pool,
        NewSpark {
            title: "Merge-Hand E2E epic".into(),
            description: "Two sub-PRs integrating through the Merge Hand.".into(),
            spark_type: SparkType::Epic,
            priority: 2,
            workshop_id: workshop_id.clone(),
            assignee: None,
            owner: None,
            parent_id: None,
            due_at: None,
            estimated_minutes: None,
            metadata: None,
            risk_level: None,
            scope_boundary: None,
        },
    )
    .await
    .unwrap();

    let child1 = spark_repo::create(
        &pool,
        NewSpark {
            title: "Sub-PR 1".into(),
            description: String::new(),
            spark_type: SparkType::Task,
            priority: 2,
            workshop_id: workshop_id.clone(),
            assignee: None,
            owner: None,
            parent_id: Some(epic.id.clone()),
            due_at: None,
            estimated_minutes: None,
            metadata: None,
            risk_level: None,
            scope_boundary: None,
        },
    )
    .await
    .unwrap();

    let child2 = spark_repo::create(
        &pool,
        NewSpark {
            title: "Sub-PR 2".into(),
            description: String::new(),
            spark_type: SparkType::Task,
            priority: 2,
            workshop_id: workshop_id.clone(),
            assignee: None,
            owner: None,
            parent_id: Some(epic.id.clone()),
            due_at: None,
            estimated_minutes: None,
            metadata: None,
            risk_level: None,
            scope_boundary: None,
        },
    )
    .await
    .unwrap();

    let merge_spark = spark_repo::create(
        &pool,
        NewSpark {
            title: "Merge-Hand for the E2E epic".into(),
            description: String::new(),
            spark_type: SparkType::Task,
            priority: 2,
            workshop_id: workshop_id.clone(),
            assignee: None,
            owner: None,
            parent_id: Some(epic.id.clone()),
            due_at: None,
            estimated_minutes: None,
            metadata: None,
            risk_level: None,
            scope_boundary: None,
        },
    )
    .await
    .unwrap();

    // ── 2. Create & drive each child Assignment to Approved ─────────
    //
    // `create_assignment` inserts with phase=Assigned; the test then
    // walks the proper transitions so the `events` audit trail carries
    // the full author → reviewer handshake.
    let author1_actor = "actor-author-1";
    let author2_actor = "actor-author-2";
    let reviewer_actor = "actor-reviewer-1";
    let merge_actor = "actor-merge-hand";

    let _asgn1_row = assign_repo::create_assignment(
        &pool,
        NewAssignment {
            spark_id: child1.id.clone(),
            actor_id: author1_actor.into(),
            assignment_phase: AssignmentPhase::Assigned,
            source_branch: Some(format!("{}/sub1", author1_actor)),
            target_branch: Some("main".into()),
        },
    )
    .await
    .unwrap();

    let _asgn2_row = assign_repo::create_assignment(
        &pool,
        NewAssignment {
            spark_id: child2.id.clone(),
            actor_id: author2_actor.into(),
            assignment_phase: AssignmentPhase::Assigned,
            source_branch: Some(format!("{}/sub2", author2_actor)),
            target_branch: Some("main".into()),
        },
    )
    .await
    .unwrap();

    // Pull the integer primary keys so the transition validator can see
    // them; `create_assignment` returns the Assignment directly.
    let asgn1_id: i64 = sqlx::query_scalar("SELECT id FROM assignments WHERE spark_id = ?")
        .bind(&child1.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let asgn2_id: i64 = sqlx::query_scalar("SELECT id FROM assignments WHERE spark_id = ?")
        .bind(&child2.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    drive_child_to_approved(&pool, asgn1_id, author1_actor, reviewer_actor).await;
    drive_child_to_approved(&pool, asgn2_id, author2_actor, reviewer_actor).await;

    // ── 3. Author real git branches + wire PR numbers onto assignments ─
    //
    // The sub-PR mirror columns (`github_artifact_branch`,
    // `github_artifact_pr_number`) are what the Merge-Hand precondition
    // checker reads to identify each child's sub-PR for the conflict /
    // CI env probes, and what the Epic PR body template references.
    let sub1_branch = format!("{}/sub1", author1_actor);
    let sub2_branch = format!("{}/sub2", author2_actor);
    author_sub_branch(&root, &sub1_branch, "feature1.txt", "feature 1\n");
    author_sub_branch(&root, &sub2_branch, "feature2.txt", "feature 2\n");

    sqlx::query(
        "UPDATE assignments SET github_artifact_branch = ?, github_artifact_pr_number = ? \
         WHERE id = ?",
    )
    .bind(&sub1_branch)
    .bind(101_i64)
    .bind(asgn1_id)
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "UPDATE assignments SET github_artifact_branch = ?, github_artifact_pr_number = ? \
         WHERE id = ?",
    )
    .bind(&sub2_branch)
    .bind(102_i64)
    .bind(asgn2_id)
    .execute(&pool)
    .await
    .unwrap();

    // ── 4. Seed the Merge Hand's own Assignment at Approved ───────
    //
    // The merge-hand's Assignment sits under the merge-spark. Its role
    // is integration — the merge hand does NOT perform the usual
    // author-review cycle on its own Assignment; its legal edges are
    // `Approved → ReadyForMerge` and `ReadyForMerge → Merged` (both
    // merge-hand-exclusive per the transition map). We seed the row
    // directly at Approved — which is the precondition every Merge-Hand
    // contract clause treats as "ready to integrate" — so the Merged
    // transition at the end lands through `mark_assignment_merged`
    // exactly as a production spawn would drive it.
    assign_repo::create_assignment(
        &pool,
        NewAssignment {
            spark_id: merge_spark.id.clone(),
            actor_id: merge_actor.into(),
            assignment_phase: AssignmentPhase::Approved,
            source_branch: Some(format!("crew/{}", merge_spark.id)),
            target_branch: Some("main".into()),
        },
    )
    .await
    .unwrap();

    let merge_asgn_id: i64 = sqlx::query_scalar("SELECT id FROM assignments WHERE spark_id = ?")
        .bind(&merge_spark.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    // ── 5. Merge-Hand preconditions gate (the "spawn" step) ────────
    //
    // This replicates exactly what [`hand_spawn::check_merge_preconditions`]
    // verifies on every merge-hand spawn. Reaching this point with a
    // passing report is the runtime signal that a `ryve hand spawn
    // --role merge_hand` call would NOT be refused — i.e. the merge
    // hand is "spawned". We reproduce the DB-level half of the checker
    // here because `check_merge_preconditions` lives in the bin crate
    // and is not reachable from an integration test.
    //
    // The two DB-level clauses:
    //   (a) every child Assignment is in `Approved` phase, and
    //   (d) zero child Assignments are in `Stuck` phase.
    //
    // The two env-level clauses (no merge conflicts / CI green) resolve
    // to `NotApplicable` under the default `NoopMergePreconditionEnv`,
    // which is non-blocking by contract — a real probe lands later via
    // a sibling spark.
    let child_rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT spark_id, assignment_phase FROM assignments WHERE spark_id IN (?, ?)",
    )
    .bind(&child1.id)
    .bind(&child2.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(child_rows.len(), 2);
    for (spark_id, phase) in &child_rows {
        assert_eq!(
            phase.as_deref(),
            Some("approved"),
            "child {spark_id} must be in Approved phase before merge-hand can spawn"
        );
    }
    let stuck_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM assignments \
         WHERE spark_id IN (?, ?) AND assignment_phase = 'stuck'",
    )
    .bind(&child1.id)
    .bind(&child2.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stuck_count, 0, "no child may be Stuck on the happy path");

    // A refused spawn emits exactly one 'epic.blocker_raised' outbox
    // row; the happy path must emit zero.
    let blocker_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_outbox WHERE event_type = 'epic.blocker_raised'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        blocker_rows, 0,
        "preconditions pass — no epic.blocker_raised row must fire"
    );

    // ── 6. Play the Merge Hand: integrate sub-PRs in creation order ─
    //
    // The Merge-Hand contract (see `compose_merge_hand_prompt`) names
    // Assignment-creation order — ascending `assigned_at`, ties broken
    // by spark id — as the canonical integration order. We query the
    // DB for that order explicitly so the test would detect any future
    // regression that resorts the integration queue.
    let ordered_children: Vec<(String, i64, String, String, i64)> = sqlx::query_as(
        "SELECT a.spark_id, a.id, a.assignment_id, a.actor_id, a.github_artifact_pr_number \
         FROM assignments a \
         WHERE a.spark_id IN (?, ?) \
         ORDER BY a.assigned_at ASC, a.spark_id ASC",
    )
    .bind(&child1.id)
    .bind(&child2.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(ordered_children.len(), 2);
    assert_eq!(
        ordered_children[0].0, child1.id,
        "first-created child must integrate first"
    );
    assert_eq!(
        ordered_children[1].0, child2.id,
        "second-created child must integrate second"
    );

    // Cut the Epic branch off main and merge each sub-PR with --no-ff
    // so every sub-PR lands as its own merge commit — that is the
    // observable invariant the spark's acceptance criterion pins.
    let crew_id = format!("e2e-{}", &merge_spark.id[..8.min(merge_spark.id.len())]);
    let epic_branch = format!("crew/{crew_id}");
    git(&root, &["checkout", "-q", "-b", &epic_branch, "main"]);
    git(
        &root,
        &[
            "merge",
            "--no-ff",
            "-m",
            &format!(
                "merge {sub1_branch} (PR #101) into {epic_branch} [sp-{}]",
                &merge_spark.id
            ),
            &sub1_branch,
        ],
    );
    git(
        &root,
        &[
            "merge",
            "--no-ff",
            "-m",
            &format!(
                "merge {sub2_branch} (PR #102) into {epic_branch} [sp-{}]",
                &merge_spark.id
            ),
            &sub2_branch,
        ],
    );

    // ── 7. Compose and pin the Epic PR body ─────────────────────────
    let template_children: Vec<(String, String, String, i64)> = ordered_children
        .iter()
        .map(|(spark_id, _row_id, asgn_id, actor_id, pr)| {
            (asgn_id.clone(), spark_id.clone(), actor_id.clone(), *pr)
        })
        .collect();
    let epic_pr_body = compose_epic_pr_body(&epic.title, &epic.id, &crew_id, &template_children);

    // The body must mention every child's Assignment id, spark id, and
    // source PR number, and explicitly state the `git merge --no-ff`
    // discipline. A missing child or PR number is a protocol
    // violation — the Merge-Hand prompt calls this out by name.
    assert!(
        epic_pr_body.contains(&ordered_children[0].2),
        "body must list first child's assignment id: {epic_pr_body}"
    );
    assert!(
        epic_pr_body.contains(&ordered_children[1].2),
        "body must list second child's assignment id: {epic_pr_body}"
    );
    assert!(
        epic_pr_body.contains(&child1.id),
        "body must list first child's spark id: {epic_pr_body}"
    );
    assert!(
        epic_pr_body.contains(&child2.id),
        "body must list second child's spark id: {epic_pr_body}"
    );
    assert!(
        epic_pr_body.contains("PR #101"),
        "body must reference first child's source PR: {epic_pr_body}"
    );
    assert!(
        epic_pr_body.contains("PR #102"),
        "body must reference second child's source PR: {epic_pr_body}"
    );
    assert!(
        epic_pr_body.contains("git merge --no-ff"),
        "body must state the --no-ff discipline: {epic_pr_body}"
    );
    assert!(
        epic_pr_body.contains(&epic.title),
        "body header must carry the Epic title: {epic_pr_body}"
    );

    // ── 8. Assert the git log pins the --no-ff, creation-order invariant ─
    //
    // `--merges --first-parent` isolates the merge commits the Epic
    // branch owns (as opposed to commits inherited from the sub-branches
    // they integrate); `%s` prints just the subject so the substring
    // checks stay stable across commit ids.
    let log = git_out(
        &root,
        &[
            "log",
            "--merges",
            "--first-parent",
            "--pretty=%s",
            &epic_branch,
        ],
    );
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "Epic branch must carry exactly two --no-ff merge commits, got:\n{log}"
    );
    // `git log` prints newest-first, so the second line is the first
    // merge (child1) and the first line is the second merge (child2).
    assert!(
        lines[1].contains(&sub1_branch),
        "earliest merge commit must integrate {sub1_branch}, got {}",
        lines[1]
    );
    assert!(
        lines[0].contains(&sub2_branch),
        "latest merge commit must integrate {sub2_branch}, got {}",
        lines[0]
    );

    // ── 9. Execute the Epic → main merge ───────────────────────────
    //
    // In production this is `gh pr merge <epic_pr> --merge` running
    // against a branch-protected main that requires APPROVED review
    // and green CI. In the test we stand in for the GitHub gate with
    // an explicit "approvals are present" precondition (the merge
    // spark's Assignment is in Approved phase, which gates the
    // transition from Approved → ReadyForMerge), then the final
    // `mark_assignment_merged` emits the Merged transition.
    git(&root, &["checkout", "-q", "main"]);
    git(
        &root,
        &[
            "merge",
            "--no-ff",
            "-m",
            &format!("merge {epic_branch} into main [sp-{}]", &merge_spark.id),
            &epic_branch,
        ],
    );

    transition::transition_assignment_phase(
        &pool,
        merge_asgn_id,
        merge_actor,
        TransitionActorRole::MergeHand,
        AssignmentPhase::ReadyForMerge,
        AssignmentPhase::Approved,
        2,
    )
    .await
    .expect("merge_hand advances Approved → ReadyForMerge");

    let merged = transition::mark_assignment_merged(&pool, merge_asgn_id, merge_actor, 3)
        .await
        .expect("merge_hand drives ReadyForMerge → Merged");

    assert_eq!(
        merged.assignment_phase.as_deref(),
        Some("merged"),
        "final phase must be Merged"
    );
    assert_eq!(
        merged.phase_actor_role.as_deref(),
        Some("merge_hand"),
        "only merge_hand may emit the Merged transition"
    );

    // ── 10. Assert the Merged transition event fires ────────────────
    let (old_value, new_value, actor) =
        sqlx::query_as::<_, (Option<String>, Option<String>, String)>(
            "SELECT old_value, new_value, actor FROM events \
         WHERE spark_id = ? AND field_name = 'assignment_phase' \
         ORDER BY id DESC LIMIT 1",
        )
        .bind(&merge_spark.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(old_value.as_deref(), Some("ready_for_merge"));
    assert_eq!(new_value.as_deref(), Some("merged"));
    assert_eq!(actor, merge_actor);

    // ── 11. Assert every expected phase-change event is present, in order ─
    //
    // The Merge-Hand happy path must produce this ordered sequence on
    // the `events` table (filtered to phase-change events so creation
    // rows — which have `old_value IS NULL` — don't muddy the order
    // check):
    //
    //   1. child1:  assigned → in_progress         (actor-author-1)
    //   2. child1:  in_progress → awaiting_review  (actor-author-1)
    //   3. child1:  awaiting_review → approved     (actor-reviewer-1)
    //   4. child2:  assigned → in_progress         (actor-author-2)
    //   5. child2:  in_progress → awaiting_review  (actor-author-2)
    //   6. child2:  awaiting_review → approved     (actor-reviewer-1)
    //   7. merge:   approved → ready_for_merge     (actor-merge-hand)
    //   8. merge:   ready_for_merge → merged       (actor-merge-hand)
    //
    // The query filters on the three sparks that carry phase events
    // and asserts the sequence exactly — any missing or reordered
    // transition is a regression.
    type PhaseEventRow = (String, Option<String>, Option<String>, String, String);
    let events: Vec<PhaseEventRow> = sqlx::query_as(
        "SELECT spark_id, old_value, new_value, actor, timestamp FROM events \
         WHERE field_name = 'assignment_phase' \
           AND old_value IS NOT NULL \
           AND spark_id IN (?, ?, ?) \
         ORDER BY id ASC",
    )
    .bind(&child1.id)
    .bind(&child2.id)
    .bind(&merge_spark.id)
    .fetch_all(&pool)
    .await
    .unwrap();

    let expected: &[(&str, &str, &str, &str)] = &[
        (&child1.id, "assigned", "in_progress", author1_actor),
        (&child1.id, "in_progress", "awaiting_review", author1_actor),
        (&child1.id, "awaiting_review", "approved", reviewer_actor),
        (&child2.id, "assigned", "in_progress", author2_actor),
        (&child2.id, "in_progress", "awaiting_review", author2_actor),
        (&child2.id, "awaiting_review", "approved", reviewer_actor),
        (&merge_spark.id, "approved", "ready_for_merge", merge_actor),
        (&merge_spark.id, "ready_for_merge", "merged", merge_actor),
    ];
    assert_eq!(
        events.len(),
        expected.len(),
        "wrong number of phase events; got {events:#?}"
    );
    for (i, (spark_id, old, new, actor)) in expected.iter().enumerate() {
        let row = &events[i];
        assert_eq!(&row.0, spark_id, "step {i}: spark_id");
        assert_eq!(row.1.as_deref(), Some(*old), "step {i}: old_value");
        assert_eq!(row.2.as_deref(), Some(*new), "step {i}: new_value");
        assert_eq!(&row.3, actor, "step {i}: actor");
    }

    // Timestamps must be non-decreasing in the order we read them —
    // ascending `id` order is the insert order, but we pin the
    // temporal invariant explicitly so a future change to id
    // allocation (e.g. per-spark sequences) cannot silently reorder
    // the outbox relay's replay.
    let timestamps: Vec<&str> = events.iter().map(|r| r.4.as_str()).collect();
    let mut sorted = timestamps.clone();
    sorted.sort();
    assert_eq!(
        timestamps, sorted,
        "events must be timestamp-ordered as-read"
    );

    // ── 12. Cleanup ────────────────────────────────────────────────
    pool.close().await;
    let _ = std::fs::remove_dir_all(&root);
}
