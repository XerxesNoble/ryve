// SPDX-License-Identifier: AGPL-3.0-or-later

//! End-to-end integration test for the investigator Hand role
//! ([sp-5b4dcbc3] / spark ryve-985e4967).
//!
//! Drives the full Ryve-side plumbing a Research Head relies on:
//!
//!   1. Create a fresh temp workshop and an audit epic with a populated
//!      `problem_statement` (so the investigator prompt has something to
//!      scope against).
//!   2. Shell out to `ryve hand spawn --role investigator`, which ends up
//!      in `spawn_hand(HandKind::Investigator, ...)` inside the binary.
//!      The stub agent is a tiny shell script that records its argv and
//!      sleeps briefly so the tmux session stays alive long enough for
//!      `pipe-pane` to attach.
//!   3. Assert the DB + filesystem shape the investigator role is
//!      supposed to produce: session_label = "investigator", assignment
//!      role = owner, crew member role label "investigator", and a
//!      prompt file under `.ryve/prompts/` that carries the read-only
//!      contract ("READ-ONLY"), the comment-based finding channel
//!      ("ryve comment add"), and the audit spark's problem statement
//!      (scoping the sweep).
//!   4. Simulate the investigator running `ryve comment add <spark>
//!      <finding>` and assert the comment persists and is readable via
//!      `ryve comment list`.
//!
//! Test is self-contained: its own tempdir, its own sparks.db pool, its
//! own stub agent. The coding-agent subprocess is always the stub — no
//! real `claude` / `codex` is launched. The test gates on a real tmux
//! binary (bundled when the build script has built
//! `vendor/tmux/bin/tmux`, or system tmux as a fallback) and skips
//! cleanly if neither is available so CI runners without tmux stay
//! green.

use std::path::{Path, PathBuf};
use std::process::Command;

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// Mirror of `src/tmux.rs::resolve_tmux_bin` — resolved here so the test
/// and the binary both agree on which tmux to use. Order: explicit env
/// overrides > bundled > system `which tmux`. Returning `None` means no
/// tmux is installed; the test will skip in that case.
fn find_tmux_binary() -> Option<PathBuf> {
    for var in ["RYVE_TMUX_PATH", "RYVE_TMUX_BIN"] {
        if let Ok(val) = std::env::var(var) {
            let p = PathBuf::from(val);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bundled = manifest.join("vendor/tmux/bin/tmux");
    if bundled.exists() {
        return Some(bundled);
    }
    let out = Command::new("which").arg("tmux").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// Build a throwaway workshop: `git init` + empty commit + `ryve init` +
/// a stub agent shell script. Mirrors `tests/tmux_lifecycle.rs` so the
/// two integration tests share one setup pattern.
fn setup_workshop() -> (PathBuf, PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ryve-investigator-test-{nanos}-{}",
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

    // Stub agent: just sleep so the tmux session stays alive long enough
    // for pipe-pane to attach (same rationale as src/hand_spawn.rs's
    // test stub). The investigator test does not inspect argv — the
    // prompt is read directly from the file `spawn_hand` wrote under
    // .ryve/prompts/ — so this does not need to record anything.
    let stub_path = root.join("stub-agent.sh");
    std::fs::write(&stub_path, "#!/bin/sh\nsleep 3\n").expect("write stub agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub_path, perms).unwrap();
    }

    (root, stub_path)
}

/// Shell out to `ryve` with the workshop root pinned via env. Always
/// pins `RYVE_TMUX_PATH` to whatever the test resolved, so the binary
/// and the test both drive the same tmux.
fn ryve(root: &Path, tmux_bin: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ryve_bin())
        .args(args)
        .current_dir(root)
        .env("RYVE_WORKSHOP_ROOT", root)
        .env("RYVE_TMUX_PATH", tmux_bin)
        .output()
        .expect("spawn ryve")
}

/// Tear down the tmux session the spawn created so the private socket
/// goes away before the tempdir is removed. Best-effort — the stub's
/// `sleep` will let it die on its own anyway.
fn kill_tmux_session(tmux_bin: &Path, root: &Path, session_name: &str) {
    let socket = expected_socket(root);
    // Swallow stderr: if the stub agent's `sleep` has already elapsed
    // and tmux tore the session down on its own, `kill-session` prints
    // "can't find session" to stderr. That is expected, not a failure.
    let _ = Command::new(tmux_bin)
        .args([
            "-S",
            &socket.to_string_lossy(),
            "kill-session",
            "-t",
            session_name,
        ])
        .stderr(std::process::Stdio::null())
        .status();
}

/// Mirror of `src/tmux.rs::short_socket_path`. Duplicated here because
/// the binary crate has no library target to import from.
fn expected_socket(workshop_dir: &Path) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let state_dir = workshop_dir.join(".ryve");
    let canonical = state_dir.join("tmux.sock");
    if canonical.to_string_lossy().len() <= 100 {
        return canonical;
    }
    let mut hasher = DefaultHasher::new();
    state_dir.hash(&mut hasher);
    let hash = hasher.finish();
    PathBuf::from(format!("/tmp/ryve-{hash:016x}.sock"))
}

/// Extract the value of `"<key>": "..."` from a flat JSON blob. Good
/// enough for the CLI's `--json` payloads which all use serde_json's
/// stable pretty output.
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

#[tokio::test]
async fn investigator_hand_end_to_end() {
    let Some(tmux_bin) = find_tmux_binary() else {
        eprintln!(
            "tmux binary not found — skipping investigator Hand integration test. \
             CI runners without bundled tmux are expected to hit this path."
        );
        return;
    };

    let (root, stub_path) = setup_workshop();

    // --- (1) Audit epic with a populated problem statement. The
    // investigator prompt composer uses `push_spark_details` to emit
    // the statement so the investigator can scope its sweep without a
    // second round-trip. Picking a statement longer than 40 chars so
    // the "first 40 chars" acceptance criterion is meaningful.
    let problem_statement = "Audit the perf-core crate for hot-path allocations and \
         unbounded channel growth under sustained ingest load.";
    let create = ryve(
        &root,
        &tmux_bin,
        &[
            "--json",
            "spark",
            "create",
            "--type",
            "epic",
            "--priority",
            "1",
            "--problem",
            problem_statement,
            "perf-core audit",
        ],
    );
    assert!(
        create.status.success(),
        "spark create failed: stdout={} stderr={}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    let create_stdout = String::from_utf8_lossy(&create.stdout);
    let spark_id = parse_json_string(&create_stdout, "id")
        .unwrap_or_else(|| panic!("could not parse spark id from: {create_stdout}"));

    // --- (2) Crew parented on the audit spark. Every investigator Hand
    // is expected to land in a Research Head's crew; spawning with
    // `--crew` exercises the `crew_repo::add_member(.., role_label)`
    // branch that must tag the investigator member with
    // role="investigator", not the default "hand".
    let crew_out = ryve(
        &root,
        &tmux_bin,
        &[
            "--json",
            "crew",
            "create",
            "--parent",
            &spark_id,
            "perf-core audit crew",
        ],
    );
    assert!(
        crew_out.status.success(),
        "crew create failed: stdout={} stderr={}",
        String::from_utf8_lossy(&crew_out.stdout),
        String::from_utf8_lossy(&crew_out.stderr)
    );
    let crew_stdout = String::from_utf8_lossy(&crew_out.stdout);
    let crew_id = parse_json_string(&crew_stdout, "id")
        .unwrap_or_else(|| panic!("could not parse crew id from: {crew_stdout}"));

    // --- (3) Spawn the investigator Hand. `hand spawn --role
    // investigator` ends up in `spawn_hand(HandKind::Investigator)`
    // inside the binary, which is the code path under test.
    let spawn = ryve(
        &root,
        &tmux_bin,
        &[
            "--json",
            "hand",
            "spawn",
            &spark_id,
            "--role",
            "investigator",
            "--agent",
            &stub_path.to_string_lossy(),
            "--crew",
            &crew_id,
            "--actor",
            "tester",
        ],
    );
    assert!(
        spawn.status.success(),
        "hand spawn failed: stdout={} stderr={}",
        String::from_utf8_lossy(&spawn.stdout),
        String::from_utf8_lossy(&spawn.stderr)
    );
    let spawn_stdout = String::from_utf8_lossy(&spawn.stdout);
    let session_id = parse_json_string(&spawn_stdout, "session_id")
        .unwrap_or_else(|| panic!("could not parse session_id from: {spawn_stdout}"));

    // --- (4) Prompt file assertions. `spawn_hand` writes the composed
    // prompt to `.ryve/prompts/hand-<session>.md` before launching the
    // agent. The investigator-specific content is what we are guarding
    // against regression.
    let prompt_path = root
        .join(".ryve")
        .join("prompts")
        .join(format!("hand-{session_id}.md"));
    let prompt = std::fs::read_to_string(&prompt_path).unwrap_or_else(|e| {
        panic!(
            "investigator prompt file missing at {}: {e}",
            prompt_path.display()
        )
    });
    assert!(
        prompt.contains("READ-ONLY"),
        "investigator prompt must state the READ-ONLY contract; got:\n{prompt}"
    );
    assert!(
        prompt.contains("ryve comment add"),
        "investigator prompt must direct findings to `ryve comment add`; got:\n{prompt}"
    );
    // Problem-statement scoping: the prompt must carry at least the
    // first 40 characters of the audit spark's problem_statement so
    // the investigator knows what it is sweeping without a second
    // round-trip to the workgraph.
    let ps_prefix: String = problem_statement.chars().take(40).collect();
    assert!(
        prompt.contains(&ps_prefix),
        "investigator prompt must include the audit spark's problem statement \
         (first 40 chars: {ps_prefix:?}); got:\n{prompt}"
    );

    // --- (5) Session-row assertions. There is no CLI today that
    // surfaces a single session's `session_label`, so the test opens
    // the workshop's sparks.db via `data::db::open_sparks_db` and
    // queries the repo directly. Read-only — every mutation in this
    // test still flows through the `ryve` CLI.
    let pool = data::db::open_sparks_db(&root)
        .await
        .expect("open sparks.db for readback");
    let session = data::sparks::agent_session_repo::get(&pool, &session_id)
        .await
        .expect("session lookup")
        .unwrap_or_else(|| panic!("agent_sessions row missing for session {session_id}"));
    assert_eq!(
        session.session_label.as_deref(),
        Some("investigator"),
        "session_label must be 'investigator'"
    );

    // --- (6) Assignment-row assertions. `ryve assign list --json`
    // serialises `AssignmentRole` as snake_case, so `AssignmentRole::Owner`
    // lands as `"owner"` in the JSON.
    let assign = ryve(&root, &tmux_bin, &["--json", "assign", "list", &spark_id]);
    assert!(
        assign.status.success(),
        "assign list failed: stdout={} stderr={}",
        String::from_utf8_lossy(&assign.stdout),
        String::from_utf8_lossy(&assign.stderr)
    );
    let assign_stdout = String::from_utf8_lossy(&assign.stdout);
    assert!(
        assign_stdout.contains(&session_id),
        "assignment must list the investigator session: {assign_stdout}"
    );
    let role = parse_json_string(&assign_stdout, "role")
        .unwrap_or_else(|| panic!("no role field in: {assign_stdout}"));
    assert_eq!(
        role, "owner",
        "investigator assignment role must be Owner (not Merger/Observer)"
    );

    // --- (7) Crew-member role-label assertions. `crew show --json`
    // returns `{ "crew": ..., "members": [...] }`; we walk the members
    // array and look for our session with role="investigator".
    let crew_show = ryve(&root, &tmux_bin, &["--json", "crew", "show", &crew_id]);
    assert!(
        crew_show.status.success(),
        "crew show failed: stdout={} stderr={}",
        String::from_utf8_lossy(&crew_show.stdout),
        String::from_utf8_lossy(&crew_show.stderr)
    );
    let crew_show_stdout = String::from_utf8_lossy(&crew_show.stdout);
    let crew_json: serde_json::Value = serde_json::from_str(&crew_show_stdout)
        .unwrap_or_else(|e| panic!("crew show returned invalid JSON ({e}): {crew_show_stdout}"));
    let members = crew_json["members"]
        .as_array()
        .unwrap_or_else(|| panic!("no members array in crew show: {crew_show_stdout}"));
    let investigator_member = members
        .iter()
        .find(|m| m["session_id"].as_str() == Some(&session_id))
        .unwrap_or_else(|| {
            panic!("session {session_id} not found in crew members: {crew_show_stdout}")
        });
    assert_eq!(
        investigator_member["role"].as_str(),
        Some("investigator"),
        "crew member role label must be 'investigator', got: {investigator_member}"
    );

    // --- (8) Comment-based finding flow. The investigator contract is
    // that findings flow ONLY through `ryve comment add`; simulate one
    // and read it back via `ryve comment list` to prove the round-trip
    // works end-to-end.
    let finding = r#"FINDING
severity: high
category: performance
location: perf_core/src/ingest.rs:87
evidence: unbounded mpsc::channel with no backpressure
recommendation: cap channel capacity or switch to bounded sender"#;
    let add = ryve(&root, &tmux_bin, &["comment", "add", &spark_id, finding]);
    assert!(
        add.status.success(),
        "comment add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );
    let list = ryve(&root, &tmux_bin, &["--json", "comment", "list", &spark_id]);
    assert!(
        list.status.success(),
        "comment list failed: stdout={} stderr={}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    let comments: serde_json::Value = serde_json::from_str(&list_stdout)
        .unwrap_or_else(|e| panic!("comment list returned invalid JSON ({e}): {list_stdout}"));
    let arr = comments
        .as_array()
        .unwrap_or_else(|| panic!("expected array from comment list: {list_stdout}"));
    assert!(
        arr.iter().any(|c| {
            c["spark_id"].as_str() == Some(&spark_id)
                && c["body"].as_str().is_some_and(|b| {
                    b.contains("FINDING") && b.contains("perf_core/src/ingest.rs:87")
                })
        }),
        "finding comment must be persisted on spark {spark_id}: {list_stdout}"
    );

    // Cleanup. Best-effort — the stub agent's `sleep` lets the tmux
    // session die naturally and a leftover socket in /tmp is harmless.
    let tmux_name = format!("hand-{session_id}");
    kill_tmux_session(&tmux_bin, &root, &tmux_name);
    drop(pool);
    let _ = std::fs::remove_dir_all(&root);
}
