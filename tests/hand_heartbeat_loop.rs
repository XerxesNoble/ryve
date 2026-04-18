// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Integration test for spark ryve-85034c27: a spawned Hand emits
// `HeartbeatReceived` events to the outbox while its assignment is
// active. We exercise the sidecar body directly (the `ryve hand
// heartbeat-loop` subcommand that `spawn_hand` launches in its own
// detached tmux session) so the test stays focused on the event
// emission path without depending on a tmux binary.
//
// Acceptance:
//   - Spawned Hand process emits a HeartbeatReceived event every
//     heartbeat_interval_secs while its assignment is active.
//   - Heartbeat loop ends cleanly when the session ends (here: when the
//     assignment is no longer active, which is the termination signal
//     the loop consumes).
//   - Assert >=2 HeartbeatReceived events on the outbox before
//     completion.

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
    let root = std::env::temp_dir().join(format!("ryve-hb-cli-{nanos}-{}", std::process::id()));
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

async fn seed_active_assignment(root: &Path) -> (String, String) {
    let pool = data::db::open_sparks_db(root).await.expect("open db");

    let ws_id = root.file_name().unwrap().to_string_lossy().to_string();

    let epic = spark_repo::create(
        &pool,
        NewSpark {
            title: "heartbeat test epic".into(),
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

    let session_id = uuid::Uuid::new_v4().to_string();
    agent_session_repo::create(
        &pool,
        &NewAgentSession {
            id: session_id.clone(),
            workshop_id: ws_id.clone(),
            agent_name: "stub".into(),
            agent_command: "echo".into(),
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
        &pool,
        NewHandAssignment {
            session_id: session_id.clone(),
            spark_id: epic.id.clone(),
            role: AssignmentRole::Owner,
            actor_id: None,
        },
    )
    .await
    .expect("assign owner");

    pool.close().await;
    (session_id, epic.id)
}

#[test]
fn heartbeat_loop_emits_multiple_heartbeat_received_events_before_assignment_completes() {
    // Acceptance for spark ryve-85034c27: assert the sidecar body emits
    // >=2 `HeartbeatReceived` events onto the outbox for a short-lived
    // Hand before its assignment terminates.
    let ws = fresh_workshop();
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let (session_id, spark_id) = rt.block_on(seed_active_assignment(&ws));

    // Run the sidecar body directly. `--interval-secs 0` makes the test
    // deterministic (no wall-clock sleep gap needed); `--max-ticks 3`
    // gives us a bounded run even if nothing else terminates the loop.
    let out = Command::new(ryve_bin())
        .args([
            "hand",
            "heartbeat-loop",
            &session_id,
            &spark_id,
            "--interval-secs",
            "0",
            "--max-ticks",
            "3",
        ])
        .current_dir(&ws)
        .env("RYVE_WORKSHOP_ROOT", &ws)
        .output()
        .expect("run heartbeat-loop");
    assert!(
        out.status.success(),
        "heartbeat-loop failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Re-open the DB and count heartbeat rows on the outbox.
    let count: i64 = rt.block_on(async {
        let pool = data::db::open_sparks_db(&ws).await.expect("reopen db");
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event_outbox \
             WHERE event_type = ? AND actor_id = ?",
        )
        .bind(data::sparks::heartbeat::HEARTBEAT_EVENT_TYPE)
        .bind(&session_id)
        .fetch_one(&pool)
        .await
        .expect("count query");
        pool.close().await;
        n
    });

    assert!(
        count >= 2,
        "expected at least 2 HeartbeatReceived rows on the outbox, got {count}"
    );

    // The loop must also have advanced `last_heartbeat_at` on the row.
    let last_heartbeat: Option<String> = rt.block_on(async {
        let pool = data::db::open_sparks_db(&ws).await.expect("reopen db");
        let v: Option<String> = sqlx::query_scalar(
            "SELECT last_heartbeat_at FROM assignments \
             WHERE session_id = ? AND spark_id = ?",
        )
        .bind(&session_id)
        .bind(&spark_id)
        .fetch_one(&pool)
        .await
        .expect("heartbeat query");
        pool.close().await;
        v
    });
    assert!(
        last_heartbeat.is_some(),
        "last_heartbeat_at must be stamped after the loop runs"
    );
}

#[test]
fn heartbeat_loop_exits_cleanly_when_assignment_becomes_inactive() {
    // Acceptance: "Heartbeat loop ends cleanly when the session ends."
    // We model that here by completing the assignment BEFORE the loop is
    // invoked — the sidecar must observe the inactive claim on its first
    // emit and exit without producing any outbox rows.
    let ws = fresh_workshop();
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let (session_id, spark_id) = rt.block_on(seed_active_assignment(&ws));

    rt.block_on(async {
        let pool = data::db::open_sparks_db(&ws).await.expect("reopen db");
        assignment_repo::complete(&pool, &session_id, &spark_id)
            .await
            .expect("complete assignment");
        pool.close().await;
    });

    let out = Command::new(ryve_bin())
        .args([
            "hand",
            "heartbeat-loop",
            &session_id,
            &spark_id,
            "--interval-secs",
            "0",
            "--max-ticks",
            "3",
        ])
        .current_dir(&ws)
        .env("RYVE_WORKSHOP_ROOT", &ws)
        .output()
        .expect("run heartbeat-loop");
    assert!(
        out.status.success(),
        "heartbeat-loop failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let count: i64 = rt.block_on(async {
        let pool = data::db::open_sparks_db(&ws).await.expect("reopen db");
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event_outbox \
             WHERE event_type = ? AND actor_id = ?",
        )
        .bind(data::sparks::heartbeat::HEARTBEAT_EVENT_TYPE)
        .bind(&session_id)
        .fetch_one(&pool)
        .await
        .expect("count query");
        pool.close().await;
        n
    });
    assert_eq!(
        count, 0,
        "no heartbeat rows must be written for an inactive assignment"
    );
}
