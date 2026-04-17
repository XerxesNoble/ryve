// SPDX-License-Identifier: AGPL-3.0-or-later
//
// [sp-312b98ad] Build orchestration invariant regression test.
//
// Failure mode being pinned: a Build Head finishes `finalize_with_merger`
// and exits. The Merger subprocess later dies without closing its merge
// spark. The `assignments` row is still `active`, but the owning
// `agent_sessions` row is `ended`. Before this spark, nothing surfaced
// that gap — the merge sat `in_progress` indefinitely (observed on
// ryve-e208c8ac merging epic ryve-18f4cec4).
//
// This test seeds exactly that orphan state in a fresh workshop, runs
// `ryve sweep stalls --json`, and asserts that the orphan is detected
// and a `flare` ember is emitted so humans and other Hands can see the
// stall. A second sweep call verifies the dedupe path (idempotent
// re-run, no duplicate ember).

use std::path::{Path, PathBuf};
use std::process::Command;

use data::sparks::types::{
    AssignmentRole, NewAgentSession, NewHandAssignment, NewSpark, SparkType,
};
use data::sparks::{agent_session_repo, assignment_repo, spark_repo};

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

fn fresh_workshop() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("ryve-sweep-stalls-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create tempdir");

    let ok = Command::new("git")
        .args(["init", "--initial-branch", "main"])
        .current_dir(&root)
        .status()
        .expect("git init")
        .success();
    assert!(ok, "git init failed in {root:?}");

    let ok = Command::new("git")
        .args(["config", "commit.gpgsign", "false"])
        .current_dir(&root)
        .status()
        .expect("git config")
        .success();
    assert!(ok, "git config commit.gpgsign failed");

    let ok = Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(&root)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .status()
        .expect("git commit")
        .success();
    assert!(ok, "git commit failed");

    let ok = Command::new(ryve_bin())
        .arg("init")
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .status()
        .expect("ryve init")
        .success();
    assert!(ok, "ryve init failed in {root:?}");
    root
}

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ryve_bin())
        .args(args)
        .current_dir(root)
        .env("RYVE_WORKSHOP_ROOT", root)
        .output()
        .expect("spawn ryve")
}

async fn seed_orphaned_merger(root: &Path) -> (String, String, String) {
    let pool = data::db::open_sparks_db(root).await.expect("open db");

    // Workshop id is derived from the workshop root's basename — mirrors
    // `Workshop::workshop_id()` and the CLI's own derivation.
    let ws_id = root.file_name().unwrap().to_string_lossy().to_string();

    // Parent epic — non-epic sparks need a parent_id (no-orphan invariant).
    let epic = spark_repo::create(
        &pool,
        NewSpark {
            title: "epic under test".into(),
            description: String::new(),
            spark_type: SparkType::Epic,
            priority: 1,
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

    let merge = spark_repo::create(
        &pool,
        NewSpark {
            title: "merge crew cr-test: integrate children".into(),
            description: String::new(),
            spark_type: SparkType::Chore,
            priority: 1,
            workshop_id: ws_id.clone(),
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
    .expect("create merge spark");

    // Merger session + assignment, then end the session to create the
    // exact orphan shape the sweep is meant to detect.
    let session_id = uuid::Uuid::new_v4().to_string();
    agent_session_repo::create(
        &pool,
        &NewAgentSession {
            id: session_id.clone(),
            workshop_id: ws_id.clone(),
            agent_name: "stub".into(),
            agent_command: "echo".into(),
            agent_args: vec![],
            session_label: Some("merger".into()),
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
        &pool,
        NewHandAssignment {
            session_id: session_id.clone(),
            spark_id: merge.id.clone(),
            role: AssignmentRole::Merger,
            actor_id: None,
        },
    )
    .await
    .expect("assign merger");

    // Flip spark into in_progress, then kill the session — this mirrors
    // the production stall: `in_progress` spark + `active` assignment +
    // `ended` session, with nobody polling.
    spark_repo::update(
        &pool,
        &merge.id,
        data::sparks::types::UpdateSpark {
            status: Some(data::sparks::types::SparkStatus::InProgress),
            ..Default::default()
        },
        "test",
    )
    .await
    .expect("set in_progress");

    agent_session_repo::end_session(&pool, &session_id)
        .await
        .expect("end session");

    pool.close().await;
    (ws_id, merge.id, session_id)
}

#[test]
fn sweep_stalls_detects_orphaned_merger_and_emits_flare_ember() {
    let ws = fresh_workshop();
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let (_ws_id, merge_id, session_id) = rt.block_on(seed_orphaned_merger(&ws));

    // First sweep: detect + emit exactly one flare ember for the orphan.
    let out = run(&ws, &["--json", "sweep", "stalls"]);
    assert!(
        out.status.success(),
        "sweep stalls failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("valid JSON output");
    assert_eq!(
        payload["orphan_count"].as_u64(),
        Some(1),
        "expected exactly one orphan, got: {payload}"
    );
    let orphans = payload["orphans"].as_array().expect("orphans array");
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0]["spark_id"].as_str(), Some(merge_id.as_str()));
    assert_eq!(orphans[0]["session_id"].as_str(), Some(session_id.as_str()));
    assert_eq!(orphans[0]["role"].as_str(), Some("merger"));
    assert_eq!(
        payload["emitted_embers"].as_array().map(|a| a.len()),
        Some(1),
        "first sweep must emit exactly one flare ember"
    );

    // Ember is visible via `ryve ember list` — this is the "visible
    // signal" the spark's acceptance criterion requires.
    let out = run(&ws, &["--json", "ember", "list"]);
    assert!(out.status.success());
    let embers: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let items = embers.as_array().expect("embers array");
    assert_eq!(items.len(), 1, "exactly one ember expected");
    assert_eq!(items[0]["ember_type"].as_str(), Some("flare"));
    let content = items[0]["content"].as_str().unwrap_or("");
    assert!(
        content.contains(&merge_id) && content.contains("stalled-claim"),
        "ember content must reference the orphaned spark and be tagged stalled-claim: {content}"
    );

    // Second sweep: still detects the orphan but dedupes the ember.
    let out = run(&ws, &["--json", "sweep", "stalls"]);
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["orphan_count"].as_u64(), Some(1));
    assert_eq!(
        payload["emitted_embers"].as_array().map(|a| a.len()),
        Some(0),
        "repeated sweeps must not spam embers"
    );

    // With --abandon, the phantom claim is cleared; a subsequent sweep
    // reports zero orphans (the assignment is no longer `active`).
    let out = run(&ws, &["--json", "sweep", "stalls", "--abandon"]);
    assert!(out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        payload["abandoned_claims"].as_array().map(|a| a.len()),
        Some(1),
        "--abandon must clear the one phantom claim"
    );

    let out = run(&ws, &["--json", "sweep", "stalls"]);
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        payload["orphan_count"].as_u64(),
        Some(0),
        "after --abandon the orphan must be gone: {payload}"
    );
}
