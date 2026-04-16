// SPDX-License-Identifier: AGPL-3.0-or-later

//! End-to-end tmux lifecycle test — [sp-0285181c].
//!
//! Exercises AC8 of the tmux epic against a real tmux binary (bundled when
//! present, otherwise system tmux). The test drives the full path a Hand
//! takes through Ryve:
//!
//!   1. Spawn a Hand via `ryve hand spawn` into a fresh workshop dir.
//!   2. Confirm a `hand-<session_id>` tmux session exists on the
//!      Ryve-private socket (`<workshop>/.ryve/tmux.sock`, or the hashed
//!      fallback under `/tmp` when the canonical path would overrun the
//!      Unix-domain-socket `sun_path` limit).
//!   3. Simulate a Ryve restart by doing *nothing* about the previous
//!      state (a new `ryve` subprocess automatically starts with no
//!      in-memory state) and then invoking `tmux::reconcile_sessions`
//!      via a new `ryve tmux reconcile --json` subcommand.
//!   4. Confirm the prior session appears in `confirmed_live` — the
//!      same signal that drives `tmux_session_live == true` in the UI
//!      (`src/app.rs:1681`).
//!   5. Confirm `TmuxClient::attach_command` for that session is a
//!      spawnable command by extracting its program+args via
//!      `ryve tmux attach-cmd`, swapping `attach-session` for
//!      `has-session`, and asserting the resulting command exits 0.
//!
//! The test skips cleanly (not fails) when no tmux binary is available,
//! so CI runners on Ubuntu without bundled tmux stay green.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// Mirror of `src/tmux.rs::resolve_tmux_bin` — we intentionally re-implement
/// resolution here (rather than importing from the binary crate, which is
/// not exposed as a library) so the test exercises the same resolution
/// order: `RYVE_TMUX_PATH` → `RYVE_TMUX_BIN` → bundled → `which tmux`.
fn find_tmux_binary() -> Option<PathBuf> {
    for var in ["RYVE_TMUX_PATH", "RYVE_TMUX_BIN"] {
        if let Ok(val) = std::env::var(var) {
            let p = PathBuf::from(val);
            if p.exists() {
                return Some(p);
            }
        }
    }
    // Bundled tmux in the dev layout.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bundled = manifest.join("vendor/tmux/bin/tmux");
    if bundled.exists() {
        return Some(bundled);
    }
    // System tmux.
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

/// Mirror of `src/tmux.rs::short_socket_path` for locating the Ryve-private
/// socket from outside the binary crate. Must be kept in sync with the
/// production helper.
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

/// Build a fresh temp workshop: git init, empty commit (so `git worktree
/// add` has a HEAD to point at), run `ryve init`, write a stub agent.
/// Returns `(workshop_root, stub_agent_path, out_path)`.
fn setup_workshop() -> (PathBuf, PathBuf, PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ryve-tmux-lifecycle-{nanos}-{}",
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
    run_git(&["commit", "-q", "--allow-empty", "-m", "init"]);

    // ryve init — creates .ryve/sparks.db.
    let status = Command::new(ryve_bin())
        .arg("init")
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .status()
        .expect("spawn ryve init");
    assert!(status.success(), "ryve init failed");

    // Stub agent: write argv to a file, then sleep 5s so the tmux session
    // stays alive long enough for `pipe-pane` to attach and for this test
    // to call reconcile. Same rationale as `src/hand_spawn.rs::setup_workshop`.
    let out_path = root.join("agent-out.txt");
    let stub_path = root.join("stub-agent.sh");
    std::fs::write(
        &stub_path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nsleep 5\n",
            out_path.display()
        ),
    )
    .expect("write stub agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub_path, perms).unwrap();
    }

    (root, stub_path, out_path)
}

/// Shell out to `ryve` with the workshop root pinned via env.
fn ryve(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ryve_bin())
        .args(args)
        .current_dir(root)
        .env("RYVE_WORKSHOP_ROOT", root)
        .output()
        .expect("spawn ryve")
}

/// Run `tmux -S <sock> has-session -t <name>` and return the exit status.
fn has_session(tmux_bin: &Path, socket: &Path, name: &str) -> std::process::ExitStatus {
    Command::new(tmux_bin)
        .args(["-S", &socket.to_string_lossy(), "has-session", "-t", name])
        .status()
        .expect("spawn tmux has-session")
}

#[test]
fn tmux_lifecycle_spawn_reconcile_attach() {
    let Some(tmux_bin) = find_tmux_binary() else {
        eprintln!(
            "tmux binary not found — skipping tmux lifecycle integration test. \
             CI runners without bundled tmux are expected to hit this path."
        );
        return;
    };

    let (root, stub_path, out_path) = setup_workshop();

    // --- (1a) Create a spark via the CLI. `ryve spark create` prints the
    //     new id to stdout; we parse it with --json for reliability.
    let create = ryve(
        &root,
        &[
            "--json",
            "spark",
            "create",
            "--type",
            "epic",
            "--priority",
            "1",
            "tmux lifecycle e2e",
        ],
    );
    assert!(
        create.status.success(),
        "spark create failed: stdout={} stderr={}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    let create_stdout = String::from_utf8_lossy(&create.stdout);
    let spark_id = parse_spark_id(&create_stdout)
        .unwrap_or_else(|| panic!("could not parse spark id from: {create_stdout}"));

    // --- (1b) Spawn a Hand against the stub agent. We pin the tmux binary
    //     we discovered so the spawn path and the test both use the same
    //     one (otherwise a bundled binary could be picked by one but not
    //     the other and the sessions would live on different sockets).
    let spawn = Command::new(ryve_bin())
        .args([
            "--json",
            "hand",
            "spawn",
            &spark_id,
            "--agent",
            &stub_path.to_string_lossy(),
        ])
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .env("RYVE_TMUX_PATH", &tmux_bin)
        .output()
        .expect("spawn ryve hand spawn");
    assert!(
        spawn.status.success(),
        "hand spawn failed: stdout={} stderr={}",
        String::from_utf8_lossy(&spawn.stdout),
        String::from_utf8_lossy(&spawn.stderr)
    );
    let spawn_stdout = String::from_utf8_lossy(&spawn.stdout);
    let session_id = parse_json_string(&spawn_stdout, "session_id")
        .unwrap_or_else(|| panic!("could not parse session_id from: {spawn_stdout}"));
    let tmux_name = format!("hand-{session_id}");

    // --- (2) Wait for the stub to actually exec (proves the tmux session
    //     is up and running the configured command), then assert
    //     hand-<session_id> exists on the private socket.
    let socket = expected_socket(&root);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !out_path.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        out_path.exists(),
        "stub agent never ran — tmux session did not come up. \
         spawn stdout:\n{spawn_stdout}"
    );
    let status = has_session(&tmux_bin, &socket, &tmux_name);
    assert!(
        status.success(),
        "has-session for {tmux_name} should exit 0 on {}",
        socket.display()
    );

    // --- (3 + 4) Simulate a Ryve restart: a *new* `ryve` subprocess
    //     starts with no in-memory state by definition. That fresh
    //     process calls `tmux::reconcile_sessions` via `ryve tmux
    //     reconcile --json`, and we assert our session landed in
    //     `confirmed_live` (the UI's tmux_session_live == true signal).
    let reconcile = Command::new(ryve_bin())
        .args(["--json", "tmux", "reconcile"])
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .env("RYVE_TMUX_PATH", &tmux_bin)
        .output()
        .expect("spawn ryve tmux reconcile");
    assert!(
        reconcile.status.success(),
        "tmux reconcile failed: stdout={} stderr={}",
        String::from_utf8_lossy(&reconcile.stdout),
        String::from_utf8_lossy(&reconcile.stderr)
    );
    let reconcile_stdout = String::from_utf8_lossy(&reconcile.stdout);
    assert!(
        reconcile_stdout.contains(&session_id),
        "session {session_id} should be confirmed_live after reconcile, \
         got:\n{reconcile_stdout}"
    );
    let confirmed_live = parse_json_array(&reconcile_stdout, "confirmed_live")
        .unwrap_or_else(|| panic!("no confirmed_live array in: {reconcile_stdout}"));
    assert!(
        confirmed_live.iter().any(|s| s == &session_id),
        "session {session_id} must be in confirmed_live: {confirmed_live:?}"
    );

    // --- (5) Build `TmuxClient::attach_command` for the session via
    //     `ryve tmux attach-cmd --json`, then swap `attach-session` for
    //     `has-session` so we can verify the resulting command is
    //     spawnable (attach-session itself needs a TTY and would error
    //     without one) — and, by exiting 0, that the session it points
    //     to actually exists on the same socket.
    let attach = Command::new(ryve_bin())
        .args(["--json", "tmux", "attach-cmd", &tmux_name])
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .env("RYVE_TMUX_PATH", &tmux_bin)
        .output()
        .expect("spawn ryve tmux attach-cmd");
    assert!(
        attach.status.success(),
        "tmux attach-cmd failed: stdout={} stderr={}",
        String::from_utf8_lossy(&attach.stdout),
        String::from_utf8_lossy(&attach.stderr)
    );
    let attach_stdout = String::from_utf8_lossy(&attach.stdout);
    let program = parse_json_string(&attach_stdout, "program")
        .unwrap_or_else(|| panic!("no program in: {attach_stdout}"));
    let argv = parse_json_array(&attach_stdout, "args")
        .unwrap_or_else(|| panic!("no args in: {attach_stdout}"));
    assert!(
        argv.iter().any(|a| a == "attach-session"),
        "attach_command should contain attach-session, got: {argv:?}"
    );
    assert!(
        argv.iter().any(|a| a == &tmux_name),
        "attach_command should target {tmux_name}, got: {argv:?}"
    );

    // Swap attach-session → has-session and run it.
    let probed: Vec<String> = argv
        .into_iter()
        .map(|a| {
            if a == "attach-session" {
                "has-session".to_string()
            } else {
                a
            }
        })
        .collect();
    let status = Command::new(&program)
        .args(&probed)
        .status()
        .expect("spawn probed attach command");
    assert!(
        status.success(),
        "attach_command (with has-session probe) should exit 0, got {status:?}"
    );

    // Cleanup: kill the tmux session so the socket goes away before the
    // tempdir is removed. Best-effort — the stub's `sleep 5` will let it
    // die naturally anyway.
    let _ = Command::new(&tmux_bin)
        .args([
            "-S",
            &socket.to_string_lossy(),
            "kill-session",
            "-t",
            &tmux_name,
        ])
        .status();
    let _ = std::fs::remove_dir_all(&root);
}

/// Minimal JSON-string scraper. We intentionally avoid a serde_json
/// dev-dep: the JSON we read here comes from our own CLI handlers so the
/// field shapes are stable, and the test file stays self-contained.
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

/// Parse a JSON array of strings for a given key.
fn parse_json_array(json: &str, key: &str) -> Option<Vec<String>> {
    let needle = format!("\"{key}\"");
    let idx = json.find(&needle)?;
    let rest = &json[idx + needle.len()..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let open = after.find('[')?;
    let close = after.find(']')?;
    let body = &after[open + 1..close];
    let mut out = Vec::new();
    let mut remaining = body;
    while let Some(q_start) = remaining.find('"') {
        let tail = &remaining[q_start + 1..];
        let q_end = tail.find('"')?;
        out.push(tail[..q_end].to_string());
        remaining = &tail[q_end + 1..];
    }
    Some(out)
}

/// Extract a spark id from `ryve --json spark create` output. The JSON
/// payload has an `"id": "ryve-xxxxxxxx"` field.
fn parse_spark_id(json: &str) -> Option<String> {
    parse_json_string(json, "id")
}
