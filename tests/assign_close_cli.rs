// SPDX-License-Identifier: AGPL-3.0-or-later
//
// [sp-9e6ef4e8] CLI integration for the mandatory-post gate on
// `ryve assign close`. Pins the acceptance criteria from the spark:
//   - close with zero chat-of-record posts fails (non-zero exit)
//   - close with at least one post succeeds and completes the
//     assignment
// The gate is DB-gated: it counts `irc_messages` rows directly (see
// `ipc::chat_of_record::count_posts_since_claim`), so the test never
// starts an IRC relay — it speaks to the database via the seeded helper
// and drives the CLI for the actual close path.

use std::path::{Path, PathBuf};
use std::process::Command;

use data::sparks::types::{
    AssignmentRole, IrcCommand, NewAgentSession, NewHandAssignment, NewIrcMessage, NewSpark,
    SparkType,
};
use data::sparks::{agent_session_repo, assignment_repo, irc_repo, spark_repo};

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

fn fresh_workshop() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("ryve-assign-close-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create tempdir");

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

/// Seed an epic spark + Hand session + active assignment. Returns
/// `(spark_id, session_id)` so the test can drive `ryve assign close`.
///
/// Why directly via the repo and not through the CLI: `ryve assign claim`
/// accepts an arbitrary session string but the mandatory-post gate and
/// the chat-of-record FK both require the session to exist in
/// `agent_sessions`. The session repo is the single supported way to
/// create that row (the CLI entry points are wired into Hand spawn,
/// which is heavier than we need for a focused gate test).
async fn seed_claim(root: &Path) -> (String, String) {
    let pool = data::db::open_sparks_db(root).await.expect("open db");
    let ws_id = root.file_name().unwrap().to_string_lossy().to_string();

    let epic = spark_repo::create(
        &pool,
        NewSpark {
            title: "assign-close test epic".into(),
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
    (epic.id, session_id)
}

/// Write one chat-of-record row tagged with `spark_id` as the target and
/// authored by `session_id`. Uses the typed repo so the FKs stay in
/// sync with the production insert path.
async fn seed_post(root: &Path, spark_id: &str, session_id: &str) {
    let pool = data::db::open_sparks_db(root).await.expect("open db");
    irc_repo::insert_message(
        &pool,
        NewIrcMessage {
            epic_id: spark_id.to_string(),
            channel: "#test-close".to_string(),
            irc_message_id: uuid::Uuid::new_v4().to_string(),
            sender_actor_id: Some(session_id.to_string()),
            command: IrcCommand::Privmsg,
            raw_text: "stopping at integration tests, next step is close".to_string(),
            structured_event_id: None,
        },
    )
    .await
    .expect("insert irc_messages row");
    pool.close().await;
}

#[test]
fn assign_close_fails_when_session_has_zero_chat_of_record_posts() {
    let ws = fresh_workshop();
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let (spark_id, session_id) = rt.block_on(seed_claim(&ws));

    let out = run(&ws, &["assign", "close", &session_id, &spark_id]);
    assert!(
        !out.status.success(),
        "assign close must fail when no posts exist; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("zero chat-of-record posts"),
        "stderr must explain the missing posts, got: {stderr}"
    );
    assert!(
        stderr.contains("on handoff"),
        "stderr must reference the \"on handoff\" mandatory-post boundary, got: {stderr}"
    );
    assert!(
        stderr.contains(&spark_id),
        "stderr must name the spark id, got: {stderr}"
    );

    // The assignment must still be `active` — the gate rejects the close
    // without mutating state, so a retry after posting is clean.
    let list = run(&ws, &["--json", "assign", "list", &spark_id]);
    assert!(list.status.success());
    let payload: serde_json::Value =
        serde_json::from_slice(&list.stdout).expect("assign list JSON");
    assert_eq!(
        payload["status"].as_str(),
        Some("active"),
        "failed close must leave the assignment active: {payload}"
    );
}

#[test]
fn assign_close_succeeds_when_session_has_at_least_one_post() {
    let ws = fresh_workshop();
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let (spark_id, session_id) = rt.block_on(seed_claim(&ws));
    rt.block_on(seed_post(&ws, &spark_id, &session_id));

    let out = run(&ws, &["assign", "close", &session_id, &spark_id]);
    assert!(
        out.status.success(),
        "assign close must succeed with \u{2265}1 post; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("assign closed"),
        "stdout must confirm the close, got: {stdout}"
    );
    assert!(
        stdout.contains(&spark_id) && stdout.contains(&session_id),
        "stdout must name the spark + session, got: {stdout}"
    );

    // The assignment is no longer `active` — subsequent `assign list`
    // reports the spark as unclaimed (no active owner row remains).
    let list = run(&ws, &["assign", "list", &spark_id]);
    assert!(list.status.success());
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_stdout.contains("unclaimed"),
        "post-close `assign list` must report the spark unclaimed, got: {list_stdout}"
    );
}
