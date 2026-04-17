// SPDX-License-Identifier: AGPL-3.0-or-later

//! End-to-end integration test for the Release Manager Hand archetype
//! [sp-2a82fee7] / spark ryve-e6713ee7.
//!
//! The Release Manager's tool policy is an allow-list enforced by the
//! CLI itself, not a prompt suggestion. This test stands up a throwaway
//! workshop, inserts an `agent_sessions` row labelled
//! `release_manager`, and then drives `ryve` with
//! `RYVE_HAND_SESSION_ID` pinned to that row to exercise the three
//! acceptance-criteria cases from the spark:
//!
//!   (a) `ryve hand spawn` — must fail with a tool-policy error.
//!   (b) `ryve comment add <non_release_spark>` — must fail.
//!   (c) `ryve release list` — must succeed.
//!
//! As a regression guard for the positive half of the allow-list, we
//! also verify that `ryve comment add <release_member_spark>` from the
//! same session succeeds — the Atlas-only comms channel.
//!
//! Bypassing `ryve hand spawn` when seeding the RM session is
//! intentional: the `spawn_hand` path launches a real coding-agent
//! subprocess via tmux, which CI runners without the bundled binary
//! would skip. The session-row shape the policy gate reads is simple
//! (`id`, `session_label = "release_manager"`) and can be inserted
//! directly through the data layer without any subprocess involvement.

use std::path::{Path, PathBuf};
use std::process::Command;

use data::sparks::agent_session_repo;
use data::sparks::types::NewAgentSession;

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// Build a throwaway workshop: `git init` + empty commit + `ryve init`.
/// Mirrors `tests/release_edit_cli.rs::fresh_workshop` so the two
/// release-surface integration tests share the same setup pattern.
fn fresh_workshop() -> PathBuf {
    // Uniqueness: test harness runs tests in parallel inside a single
    // process, so (SystemTime + pid) is not enough — two #[tokio::test]
    // calls that fire on the same nanosecond would try to `git init`
    // into the same directory. Appending a UUID makes the name
    // collision-free across parallel runs.
    let uuid = uuid::Uuid::new_v4();
    let root = std::env::temp_dir().join(format!(
        "ryve-release-manager-test-{}-{uuid}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create workshop tempdir");

    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&root)
            .env("GIT_AUTHOR_NAME", "ryve-test")
            .env("GIT_AUTHOR_EMAIL", "test@ryve.local")
            .env("GIT_COMMITTER_NAME", "ryve-test")
            .env("GIT_COMMITTER_EMAIL", "test@ryve.local")
            .status()
            .expect("spawn git");
        assert!(status.success(), "git {args:?} failed in {root:?}");
    };
    run_git(&["init", "-q", "-b", "main"]);
    run_git(&["config", "commit.gpgsign", "false"]);
    run_git(&["commit", "-q", "--allow-empty", "-m", "init"]);

    let status = Command::new(ryve_bin())
        .arg("init")
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .status()
        .expect("spawn ryve init");
    assert!(status.success(), "ryve init failed");

    root
}

/// Shell out to `ryve` with the workshop root pinned. When
/// `rm_session` is `Some`, set `RYVE_HAND_SESSION_ID` so the binary's
/// archetype gate resolves the caller to `release_manager`.
fn ryve_as(root: &Path, rm_session: Option<&str>, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(ryve_bin());
    cmd.args(args)
        .current_dir(root)
        .env("RYVE_WORKSHOP_ROOT", root);
    if let Some(s) = rm_session {
        cmd.env("RYVE_HAND_SESSION_ID", s);
    } else {
        cmd.env_remove("RYVE_HAND_SESSION_ID");
    }
    cmd.output().expect("spawn ryve")
}

/// Extract a JSON `"id"` value from a flat JSON blob. Good enough for
/// the CLI's `--json` payloads which all use serde_json pretty output.
fn parse_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let idx = json.find(&needle)?;
    let rest = &json[idx + needle.len()..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let quote_start = after.find('"')?;
    let after_quote = &after[quote_start + 1..];
    let quote_end = after_quote.find('"')?;
    Some(after_quote[..quote_end].to_string())
}

/// Extract `rel-<id>` from the pretty banner that `ryve release create`
/// prints on stdout (shape: "created rel-<id> v<version> …").
fn parse_release_id(stdout: &str) -> String {
    stdout
        .split_whitespace()
        .find(|t| t.starts_with("rel-"))
        .unwrap_or_else(|| panic!("no rel-<id> in stdout: {stdout}"))
        .to_string()
}

fn workshop_id_of(root: &Path) -> String {
    root.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn release_manager_policy_gate_end_to_end() {
    let ws = fresh_workshop();

    // ─── (0) Seed state the policy gate will read. ────────────────

    // A release with one member epic. The RM comment allow-list key
    // off `release_epics`, so we need at least one row there.
    let release_out = ryve_as(&ws, None, &["release", "create", "minor"]);
    assert!(
        release_out.status.success(),
        "release create failed: stdout={} stderr={}",
        String::from_utf8_lossy(&release_out.stdout),
        String::from_utf8_lossy(&release_out.stderr)
    );
    let release_id = parse_release_id(&String::from_utf8_lossy(&release_out.stdout));

    let member_out = ryve_as(
        &ws,
        None,
        &[
            "--json",
            "spark",
            "create",
            "--type",
            "epic",
            "--priority",
            "1",
            "--problem",
            "release member epic",
            "release-member-epic",
        ],
    );
    assert!(
        member_out.status.success(),
        "spark create (release member) failed: stdout={} stderr={}",
        String::from_utf8_lossy(&member_out.stdout),
        String::from_utf8_lossy(&member_out.stderr)
    );
    let member_spark = parse_json_string(&String::from_utf8_lossy(&member_out.stdout), "id")
        .expect("member spark id");

    let add_out = ryve_as(
        &ws,
        None,
        &["release", "add-epic", &release_id, &member_spark],
    );
    assert!(
        add_out.status.success(),
        "release add-epic failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add_out.stdout),
        String::from_utf8_lossy(&add_out.stderr)
    );

    // An unrelated spark the RM should NOT be able to comment on.
    // Non-epic sparks require `--parent`, so create this one as an
    // epic — the RM policy gate keys off `release_epics` membership
    // (this spark is not a member of any release), not the spark type.
    let non_member_out = ryve_as(
        &ws,
        None,
        &[
            "--json",
            "spark",
            "create",
            "--type",
            "epic",
            "--priority",
            "2",
            "unrelated work",
        ],
    );
    assert!(
        non_member_out.status.success(),
        "spark create (unrelated) failed: stdout={} stderr={}",
        String::from_utf8_lossy(&non_member_out.stdout),
        String::from_utf8_lossy(&non_member_out.stderr)
    );
    let non_member_spark =
        parse_json_string(&String::from_utf8_lossy(&non_member_out.stdout), "id")
            .expect("non-member spark id");

    // Insert the Release Manager agent_sessions row directly through
    // the data layer. The CLI reads `session_label` to resolve the
    // caller archetype — no tmux / real subprocess involved.
    let pool = data::db::open_sparks_db(&ws).await.expect("open sparks.db");
    let rm_session_id = uuid::Uuid::new_v4().to_string();
    agent_session_repo::create(
        &pool,
        &NewAgentSession {
            id: rm_session_id.clone(),
            workshop_id: workshop_id_of(&ws),
            agent_name: "stub".into(),
            agent_command: "/bin/true".into(),
            agent_args: Vec::new(),
            session_label: Some("release_manager".into()),
            child_pid: None,
            resume_id: None,
            log_path: None,
            parent_session_id: None,
            archetype_id: Some("release_manager".into()),
        },
    )
    .await
    .expect("insert release_manager agent_sessions row");
    drop(pool);

    // ─── (a) `ryve hand spawn` — MUST fail. ─────────────────────

    let spawn = ryve_as(
        &ws,
        Some(&rm_session_id),
        &["hand", "spawn", &member_spark, "--role", "owner"],
    );
    assert!(
        !spawn.status.success(),
        "release manager must NOT be able to spawn a Hand; \
         got stdout={} stderr={}",
        String::from_utf8_lossy(&spawn.stdout),
        String::from_utf8_lossy(&spawn.stderr),
    );
    let spawn_err = String::from_utf8_lossy(&spawn.stderr);
    assert!(
        spawn_err.contains("release_manager")
            && (spawn_err.contains("spawning a hand") || spawn_err.contains("forbidden")),
        "spawn refusal must attribute to the release_manager archetype: {spawn_err}"
    );

    // ─── (b) `ryve comment add <non_release>` — MUST fail. ──────

    let bad_comment = ryve_as(
        &ws,
        Some(&rm_session_id),
        &[
            "comment",
            "add",
            &non_member_spark,
            "trying to reach a non-Atlas channel",
        ],
    );
    assert!(
        !bad_comment.status.success(),
        "release manager must NOT be able to comment on a non-release spark; \
         got stdout={} stderr={}",
        String::from_utf8_lossy(&bad_comment.stdout),
        String::from_utf8_lossy(&bad_comment.stderr),
    );
    let comment_err = String::from_utf8_lossy(&bad_comment.stderr);
    assert!(
        comment_err.contains("release_manager") && comment_err.contains(&non_member_spark),
        "comment refusal must cite archetype + spark id: {comment_err}"
    );

    // ─── (c) `ryve release list` — MUST succeed. ────────────────

    let list = ryve_as(&ws, Some(&rm_session_id), &["release", "list"]);
    assert!(
        list.status.success(),
        "release manager must be allowed to run `ryve release list`; \
         got stdout={} stderr={}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr),
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_stdout.contains(&release_id),
        "release list must include the seeded release: {list_stdout}"
    );

    // ─── Regression: comment on a release member spark succeeds. ─

    let good_comment = ryve_as(
        &ws,
        Some(&rm_session_id),
        &[
            "comment",
            "add",
            &member_spark,
            "release progress update for Atlas",
        ],
    );
    assert!(
        good_comment.status.success(),
        "release manager MUST be able to comment on a release member spark; \
         got stdout={} stderr={}",
        String::from_utf8_lossy(&good_comment.stdout),
        String::from_utf8_lossy(&good_comment.stderr),
    );

    // ─── `ryve head spawn` is forbidden too. ────────────────────

    let head_spawn = ryve_as(
        &ws,
        Some(&rm_session_id),
        &["head", "spawn", &member_spark, "--archetype", "build"],
    );
    assert!(
        !head_spawn.status.success(),
        "release manager must NOT be able to spawn a Head; \
         got stdout={} stderr={}",
        String::from_utf8_lossy(&head_spawn.stdout),
        String::from_utf8_lossy(&head_spawn.stderr),
    );

    // ─── `ryve ember send` is forbidden too. ────────────────────

    let ember = ryve_as(
        &ws,
        Some(&rm_session_id),
        &["ember", "send", "flare", "this should not broadcast"],
    );
    assert!(
        !ember.status.success(),
        "release manager must NOT be able to broadcast embers; \
         got stdout={} stderr={}",
        String::from_utf8_lossy(&ember.stdout),
        String::from_utf8_lossy(&ember.stderr),
    );

    // Cleanup.
    let _ = std::fs::remove_dir_all(&ws);
}

/// Regression: without `RYVE_HAND_SESSION_ID` (direct human CLI use)
/// the default caller is unrestricted — every command the Release
/// Manager gate would forbid must still succeed. Guards against a bug
/// that accidentally applies the RM allow-list to everyone.
#[tokio::test]
async fn direct_cli_use_is_never_gated_by_release_manager_policy() {
    let ws = fresh_workshop();

    // Comment add on an arbitrary spark with no session id set.
    let create = ryve_as(
        &ws,
        None,
        &[
            "--json",
            "spark",
            "create",
            "--type",
            "epic",
            "--priority",
            "2",
            "human-driven task",
        ],
    );
    assert!(create.status.success());
    let spark_id = parse_json_string(&String::from_utf8_lossy(&create.stdout), "id").unwrap();

    let comment = ryve_as(
        &ws,
        None,
        &["comment", "add", &spark_id, "plain human note"],
    );
    assert!(
        comment.status.success(),
        "direct CLI comment must not be gated: stdout={} stderr={}",
        String::from_utf8_lossy(&comment.stdout),
        String::from_utf8_lossy(&comment.stderr),
    );

    let _ = std::fs::remove_dir_all(&ws);
}
