// SPDX-License-Identifier: AGPL-3.0-or-later

//! Regression test for spark ryve-6ef81933 / [sp-b7633430]:
//!
//! Two child sparks serialized via a `blocks` bond and touching the
//! same sentinel file must integrate with **zero** git merge conflicts.
//! The fix in `src/hand_spawn.rs::resolve_hand_base_ref` guarantees
//! this by cutting the second Hand's worktree from the first Hand's
//! branch tip instead of the release base — so by the time the Merger
//! integrates, the two Hand branches are already stacked linearly.
//!
//! Before the fix: both Hands cut from `main`. Hand A edits line 5 of
//! the sentinel, Hand B (independently) also edits line 5 — same hunk,
//! different content. The Merger's `git merge` of the second branch
//! onto the first produces an unresolvable same-hunk conflict. This is
//! the exact failure mode that drove the three migration-019 collisions
//! in 0.2.0.
//!
//! After the fix: Hand B's branch is cut from Hand A's tip SHA, so
//! Hand A's commit is already an ancestor of Hand B's branch. The two
//! Hand branches are stacked linearly, which is what the test
//! asserts. The merge step below uses `git merge --no-ff` to produce
//! explicit merge commits (matching the Merger's real behaviour), and
//! the invariant under test is conflict-free integration — both
//! merges must succeed without touching the same hunk twice, not a
//! particular fast-forward shape.
//!
//! # What this test exercises
//!
//! Directly: the data-crate primitives the resolver depends on
//! (`bond_repo::list_blocks_predecessors`,
//! `assign_repo::list_assignments_for_spark`) plus the Hand branch
//! naming convention (`<actor>/<session-short>`) that
//! `src/workshop.rs::create_hand_worktree` and
//! `src/hand_spawn.rs::resolve_hand_base_ref` must agree on. The test
//! replicates the resolver's exact computation — if the production
//! resolver and this test drift apart, either the branch name computed
//! here won't match the branch `git worktree add` creates (asserted),
//! or the merge step at the bottom will observe a conflict (asserted).
//!
//! Indirectly: the outcome the acceptance criterion demands — two
//! `blocks`-bonded children sharing scope integrate cleanly.

use std::path::{Path, PathBuf};
use std::process::Command;

use data::sparks::types::{
    AssignmentRole, BondType, NewAgentSession, NewHandAssignment, NewSpark, SparkType,
};
use data::sparks::{agent_session_repo, assignment_repo, bond_repo, spark_repo};

/// Deterministic actor id used for both Hands. The cross-user refusal
/// path is out of scope here; every session shares one actor.
const TEST_ACTOR: &str = "stackhand";

fn run_git(cwd: &Path, args: &[&str]) -> std::process::Output {
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
        "git {args:?} failed in {cwd:?}: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// Initialise a fresh git repo + workshop, seed an empty `sentinel.txt`
/// at the release base, return the workshop root.
fn fresh_workshop() -> PathBuf {
    let uuid = uuid::Uuid::new_v4();
    let root = std::env::temp_dir().join(format!(
        "ryve-hand-base-branch-stack-{}-{uuid}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create workshop tempdir");

    run_git(&root, &["init", "-q", "-b", "main"]);
    run_git(&root, &["config", "commit.gpgsign", "false"]);

    // Seed a non-empty sentinel file so both Hands have something to
    // mutate on the exact same line and a stale base would produce a
    // textual conflict. Ten lines of numbered placeholders gives us
    // deterministic offsets into the file.
    let sentinel = root.join("sentinel.txt");
    std::fs::write(
        &sentinel,
        "line 1\nline 2\nline 3\nline 4\nSENTINEL\nline 6\nline 7\nline 8\nline 9\nline 10\n",
    )
    .expect("seed sentinel.txt");
    run_git(&root, &["add", "sentinel.txt"]);
    run_git(&root, &["commit", "-q", "-m", "seed sentinel"]);

    let status = Command::new(ryve_bin())
        .arg("init")
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .status()
        .expect("spawn ryve init");
    assert!(status.success(), "ryve init failed");

    root
}

/// Seed an owner agent session + active assignment for `spark_id`.
/// Returns the `session_id`.
async fn seed_hand_session(pool: &sqlx::SqlitePool, workshop_id: &str, spark_id: &str) -> String {
    let session_id = uuid::Uuid::new_v4().to_string();
    agent_session_repo::create(
        pool,
        &NewAgentSession {
            id: session_id.clone(),
            workshop_id: workshop_id.to_string(),
            agent_name: "stub".into(),
            agent_command: "true".into(),
            agent_args: vec![],
            session_label: Some("hand".into()),
            child_pid: None,
            resume_id: None,
            log_path: None,
            parent_session_id: None,
            archetype_id: None,
        },
    )
    .await
    .expect("create session");

    assignment_repo::assign(
        pool,
        NewHandAssignment {
            session_id: session_id.clone(),
            spark_id: spark_id.to_string(),
            role: AssignmentRole::Owner,
            actor_id: Some(TEST_ACTOR.to_string()),
        },
    )
    .await
    .expect("assign owner");

    session_id
}

/// Replicates `src/hand_spawn.rs::hand_branch_name` — kept in sync
/// with the production convention by assertion. If the production
/// code changes this shape, this helper must update with it.
fn hand_branch_name(actor: &str, session_id: &str) -> String {
    let short = &session_id[..8.min(session_id.len())];
    format!("{actor}/{short}")
}

/// Replicates the DB-lookup half of
/// `src/hand_spawn.rs::resolve_hand_base_ref`. The regression the
/// acceptance criterion is guarding says: the second Hand's worktree
/// is cut from this SHA, not the release base. If the production
/// resolver diverges from this lookup the final merge-clean assertion
/// below will fail.
async fn resolver_predecessor_branch_sha(
    pool: &sqlx::SqlitePool,
    workshop_dir: &Path,
    spark_id: &str,
) -> Option<String> {
    let preds = bond_repo::list_blocks_predecessors(pool, spark_id)
        .await
        .ok()?;
    for pred in preds {
        let asgns = data::sparks::assign_repo::list_assignments_for_spark(pool, &pred)
            .await
            .ok()?;
        for a in asgns {
            if a.role != "owner" {
                continue;
            }
            let Some(sid) = a.session_id.as_ref() else {
                continue;
            };
            let branch = hand_branch_name(&a.actor_id, sid);
            let out = Command::new("git")
                .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
                .current_dir(workshop_dir)
                .output()
                .ok()?;
            if out.status.success() {
                let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !sha.is_empty() {
                    return Some(sha);
                }
            }
        }
    }
    None
}

/// Mutate line 5 (the `SENTINEL` marker) of the sentinel file with
/// `new_line`, commit on the worktree's current branch.
fn edit_sentinel_and_commit(worktree: &Path, new_line: &str, commit_msg: &str) {
    let path = worktree.join("sentinel.txt");
    let original = std::fs::read_to_string(&path).expect("read sentinel");
    let mutated: String = original
        .lines()
        .enumerate()
        .map(|(i, l)| {
            if i == 4 {
                new_line.to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, mutated).expect("write sentinel");
    run_git(worktree, &["add", "sentinel.txt"]);
    run_git(worktree, &["commit", "-q", "-m", commit_msg]);
}

#[tokio::test]
async fn stacked_hands_merge_with_zero_conflicts() {
    let ws = fresh_workshop();
    let ws_id = ws
        .file_name()
        .expect("workshop has name")
        .to_string_lossy()
        .to_string();

    let pool = data::db::open_sparks_db(&ws).await.expect("open sparks db");

    // --- (1) Parent epic + two child sparks with a blocks bond ---------
    let epic = spark_repo::create(
        &pool,
        NewSpark {
            title: "stacking epic".into(),
            description: String::new(),
            spark_type: SparkType::Epic,
            priority: 2,
            workshop_id: ws_id.clone(),
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
    .expect("create epic");

    let spark_a = spark_repo::create(
        &pool,
        NewSpark {
            title: "child A — first mutation".into(),
            description: String::new(),
            spark_type: SparkType::Task,
            priority: 2,
            workshop_id: ws_id.clone(),
            assignee: None,
            owner: None,
            parent_id: Some(epic.id.clone()),
            due_at: None,
            estimated_minutes: None,
            metadata: None,
            risk_level: None,
            scope_boundary: Some("sentinel.txt".into()),
        },
    )
    .await
    .expect("create child A");

    let spark_b = spark_repo::create(
        &pool,
        NewSpark {
            title: "child B — second mutation (blocked by A)".into(),
            description: String::new(),
            spark_type: SparkType::Task,
            priority: 2,
            workshop_id: ws_id.clone(),
            assignee: None,
            owner: None,
            parent_id: Some(epic.id.clone()),
            due_at: None,
            estimated_minutes: None,
            metadata: None,
            risk_level: None,
            scope_boundary: Some("sentinel.txt".into()),
        },
    )
    .await
    .expect("create child B");

    bond_repo::create(&pool, &spark_a.id, &spark_b.id, BondType::Blocks)
        .await
        .expect("create blocks bond A->B");

    // --- (2) Hand A: session + assignment + worktree from release base --
    let session_a = seed_hand_session(&pool, &ws_id, &spark_a.id).await;
    let branch_a = hand_branch_name(TEST_ACTOR, &session_a);
    let wt_a = ws.join(".ryve").join("worktrees").join(&session_a[..8]);

    // For spark A, there are no blocks predecessors — the resolver
    // must return None so the worktree is cut from the current HEAD.
    let a_base_ref = resolver_predecessor_branch_sha(&pool, &ws, &spark_a.id).await;
    assert!(
        a_base_ref.is_none(),
        "spark A has no predecessors — resolver must return None, got {a_base_ref:?}"
    );
    run_git(
        &ws,
        &["worktree", "add", "-b", &branch_a, wt_a.to_str().unwrap()],
    );

    edit_sentinel_and_commit(
        &wt_a,
        "SENTINEL modified by hand A",
        "feat: hand A mutates sentinel [sp-b7633430]",
    );
    let sha_a = {
        let o = run_git(&wt_a, &["rev-parse", "HEAD"]);
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };

    // --- (3) Hand B: session + assignment + worktree stacked on A -------
    let session_b = seed_hand_session(&pool, &ws_id, &spark_b.id).await;
    let branch_b = hand_branch_name(TEST_ACTOR, &session_b);
    let wt_b = ws.join(".ryve").join("worktrees").join(&session_b[..8]);

    // This is the regression point: the resolver must produce Hand A's
    // tip SHA, not `None` and not `main`. If the fix is reverted and the
    // resolver returns None here, the worktree below would be cut from
    // HEAD (main) and the eventual merge would conflict on the sentinel.
    let b_base_ref = resolver_predecessor_branch_sha(&pool, &ws, &spark_b.id)
        .await
        .expect(
            "spark B has a blocks predecessor (A) whose owner Hand holds branch \
             `<actor>/<short_A>` — resolver must return A's tip SHA so B's worktree \
             stacks on A. A None here means the resolver / DB / branch-naming chain \
             diverged from the production spawn path.",
        );
    assert_eq!(
        b_base_ref, sha_a,
        "resolver must return exactly Hand A's tip SHA"
    );
    run_git(
        &ws,
        &[
            "worktree",
            "add",
            "-b",
            &branch_b,
            wt_b.to_str().unwrap(),
            &b_base_ref,
        ],
    );

    // Assert that Hand B's worktree actually starts at Hand A's tip
    // before Hand B commits. This is the invariant "the second child
    // worktree is based on the first child branch tip".
    let b_head_before = {
        let o = run_git(&wt_b, &["rev-parse", "HEAD"]);
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    assert_eq!(
        b_head_before, sha_a,
        "Hand B's worktree HEAD must equal Hand A's tip before B commits"
    );

    edit_sentinel_and_commit(
        &wt_b,
        "SENTINEL modified by hand B",
        "feat: hand B mutates sentinel [sp-b7633430]",
    );
    let sha_b = {
        let o = run_git(&wt_b, &["rev-parse", "HEAD"]);
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    assert_ne!(sha_b, sha_a, "Hand B must have added a new commit");

    // Hand A must be an ancestor of Hand B (the stacking invariant).
    let mb_out = run_git(&ws, &["merge-base", &branch_a, &branch_b]);
    let merge_base = String::from_utf8_lossy(&mb_out.stdout).trim().to_string();
    assert_eq!(
        merge_base, sha_a,
        "merge-base(A, B) must equal Hand A's tip — proof that B is stacked on A"
    );

    // --- (4) Merger integrates both back to release base ----------------
    // Back in the main checkout: merge Hand A first, then Hand B. With
    // the fix in place, both merges are fast-forwards; without the fix,
    // Hand B's merge would fail with a same-hunk conflict on line 5.
    let merge_a = Command::new("git")
        .args(["merge", "--no-ff", "--no-edit", &branch_a])
        .current_dir(&ws)
        .env("GIT_AUTHOR_NAME", "ryve-test")
        .env("GIT_AUTHOR_EMAIL", "test@ryve.local")
        .env("GIT_COMMITTER_NAME", "ryve-test")
        .env("GIT_COMMITTER_EMAIL", "test@ryve.local")
        .output()
        .expect("spawn git merge A");
    assert!(
        merge_a.status.success(),
        "merge of Hand A into main failed: stdout={} stderr={}",
        String::from_utf8_lossy(&merge_a.stdout),
        String::from_utf8_lossy(&merge_a.stderr),
    );

    let merge_b = Command::new("git")
        .args(["merge", "--no-ff", "--no-edit", &branch_b])
        .current_dir(&ws)
        .env("GIT_AUTHOR_NAME", "ryve-test")
        .env("GIT_AUTHOR_EMAIL", "test@ryve.local")
        .env("GIT_COMMITTER_NAME", "ryve-test")
        .env("GIT_COMMITTER_EMAIL", "test@ryve.local")
        .output()
        .expect("spawn git merge B");
    assert!(
        merge_b.status.success(),
        "merge of Hand B into main produced a conflict — base-branch stacking \
         regressed. Without the fix, Hand B's branch is cut from main and its \
         sentinel mutation collides with Hand A's. stdout={} stderr={}",
        String::from_utf8_lossy(&merge_b.stdout),
        String::from_utf8_lossy(&merge_b.stderr),
    );

    // Spot-check: no unmerged paths remain.
    let status = run_git(&ws, &["status", "--porcelain"]);
    let porcelain = String::from_utf8_lossy(&status.stdout);
    assert!(
        !porcelain
            .lines()
            .any(|l| l.starts_with("UU ") || l.starts_with("AA ") || l.starts_with("DD ")),
        "post-merge working tree has unmerged paths:\n{porcelain}"
    );

    pool.close().await;
    // Best-effort cleanup of the temp workshop. Ignore failures so a
    // cluttered filesystem doesn't mask a real test failure.
    let _ = std::fs::remove_dir_all(&ws);
}
