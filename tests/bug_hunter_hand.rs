// SPDX-License-Identifier: AGPL-3.0-or-later

//! Smoke spawn integration test for the Bug Hunter Hand archetype
//! [sp-1471f46a] / spark ryve-e5688777.
//!
//! Acceptance criterion from the spark:
//!
//! > Smoke spawn against a synthetic failing test produces expected
//! > behaviour under the configured tool policy (no write-denied
//! > errors).
//!
//! Bug Hunter is write-capable: its acceptance bar is "failing test →
//! passing test + smallest possible diff", which requires editing
//! code. This test drives the real `ryve hand spawn --role bug_hunter`
//! CLI path against a stub agent that exercises a Bug Hunter's core
//! filesystem operations — opening/rewriting an existing file and
//! creating a new regression-test file — and asserts that:
//!
//!   - `ryve hand spawn --role bug_hunter` succeeds (exit 0, no
//!     tool-policy error).
//!   - The stub's writes actually land on disk in the new worktree
//!     (no `PermissionDenied` from the kernel — the Bug Hunter
//!     archetype's filesystem policy must resolve to write-capable).
//!   - The persisted `agent_sessions` row carries
//!     `session_label = "bug_hunter"`.
//!
//! The stub is intentionally minimal: the Bug Hunter's language /
//! framework choices are out of scope at the archetype layer. What we
//! verify here is the spawn-path plumbing, not the hunt logic.
//!
//! Gated on the bundled tmux binary being available (same as the
//! `src/hand_spawn.rs` integration tests) — CI jobs without it skip.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use data::sparks::agent_session_repo;

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// The bundled tmux binary lives at `vendor/tmux/bin/tmux` inside the
/// workspace root. The spawn path walks up from `CARGO_MANIFEST_DIR`
/// looking for it, but at test time we can resolve it directly — and
/// if it's absent, the test skips gracefully to match the
/// `hand_spawn.rs` integration-test discipline.
fn bundled_tmux_available() -> bool {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidate = manifest.join("vendor/tmux/bin/tmux");
    candidate.exists()
}

fn workshop_id_of(root: &Path) -> String {
    root.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Throwaway workshop: `git init` + empty seed commit + `ryve init`.
/// Mirrors `tests/release_manager_hand.rs::fresh_workshop`.
fn fresh_workshop() -> PathBuf {
    let uuid = uuid::Uuid::new_v4();
    let root = std::env::temp_dir().join(format!(
        "ryve-bug-hunter-test-{}-{uuid}",
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
        assert!(status.success(), "git {args:?} failed");
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

/// Stub agent that simulates a Bug Hunter's core filesystem ops
/// against the synthetic failing test. It:
///
///   1. Writes `BUG_HUNTER_FIX.txt` (symbolic "the fix").
///   2. Creates `tests/regression_test.txt` (symbolic "the regression
///      test").
///
/// If any write fails (e.g. the worktree was chmod'd read-only by a
/// broken tool policy), the stub records the failure so the test can
/// assert on it. Sleeps briefly so `tmux pipe-pane` has time to
/// attach — matches the stub shape in `hand_spawn.rs`.
fn write_smoke_stub(workshop_dir: &Path) -> PathBuf {
    let stub_path = workshop_dir.join("stub-bug-hunter-agent.sh");
    std::fs::write(
        &stub_path,
        "#!/bin/sh\n\
         set -u\n\
         failures=\"\"\n\
         \n\
         # 1. Write the 'fix' file.\n\
         if ! echo 'bug hunter fix' > \"$PWD/BUG_HUNTER_FIX.txt\" 2>/dev/null; then\n\
             failures=\"${failures}fix-write-denied \"\n\
         fi\n\
         \n\
         # 2. Create a regression test file under tests/.\n\
         mkdir -p \"$PWD/tests\" 2>/dev/null\n\
         if ! echo 'regression test' > \"$PWD/tests/regression_test.txt\" 2>/dev/null; then\n\
             failures=\"${failures}test-write-denied \"\n\
         fi\n\
         \n\
         # 3. Record outcome.\n\
         if [ -n \"$failures\" ]; then\n\
             echo \"SMOKE_FAILED: $failures\" > \"$PWD/bug-hunter-smoke.txt\"\n\
         else\n\
             echo \"SMOKE_OK\" > \"$PWD/bug-hunter-smoke.txt\"\n\
         fi\n\
         \n\
         # Keep the pane alive for pipe-pane.\n\
         sleep 3\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub_path, perms).unwrap();
    }
    stub_path
}

/// Extract a JSON `"<key>"` string value from a flat JSON blob. Good
/// enough for the CLI's `--json` payloads which all use serde_json
/// pretty output. Same helper shape as
/// `tests/release_manager_hand.rs::parse_json_string`.
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

// No explicit tmux cleanup: the stub sleeps 3 seconds then exits,
// which takes the tmux session with it. `remove_dir_all(&ws)` at the
// end of the test collects any residual state. Skipping a manual
// `kill-session` here also avoids the "socket path too long" error
// that surfaced with the long temp-dir prefixes cargo-test uses.

/// Primary acceptance test for ryve-e5688777: a Bug Hunter spawn via
/// `ryve hand spawn --role bug_hunter` against a synthetic failing
/// test must produce expected behaviour under the configured tool
/// policy — no write-denied errors, writes land on disk, session
/// persisted with the archetype's label.
#[tokio::test]
async fn bug_hunter_smoke_spawn_is_write_capable_and_labels_session() {
    if !bundled_tmux_available() {
        eprintln!(
            "bundled tmux not available — skipping bug-hunter smoke test \
             (production spawn path is hardened against the pinned bundled \
             tmux; arbitrary system tmux is covered by the separate \
             Bundled tmux CI job)"
        );
        return;
    }

    let ws = fresh_workshop();
    let stub_path = write_smoke_stub(&ws);

    // Create a synthetic bug epic the Bug Hunter will own. Epics are
    // the only spark type that doesn't require `--parent`, and the
    // archetype doesn't care about spark_type — the policy and prompt
    // key off the assignment role and HandKind, not the spark type.
    let create = Command::new(ryve_bin())
        .env("RYVE_WORKSHOP_ROOT", &ws)
        .args([
            "--json",
            "spark",
            "create",
            "--type",
            "epic",
            "--priority",
            "2",
            "--problem",
            "reproducer for the bug-hunter smoke integration test",
            "bug: synthetic failing test for smoke spawn",
        ])
        .current_dir(&ws)
        .output()
        .expect("spawn ryve spark create");
    assert!(
        create.status.success(),
        "spark create failed: stdout={} stderr={}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    let spark_id = parse_json_string(&String::from_utf8_lossy(&create.stdout), "id")
        .expect("spark id in --json output");

    // Register the stub as a coding-agent so `--agent stub` resolves.
    // Direct `ryve hand spawn --agent <path>` isn't supported — the
    // CLI calls `resolve_agent` which looks in the registered list
    // and on $PATH. Simplest: symlink the stub onto a `claude` name
    // inside a temporary PATH entry.
    let bin_dir = ws.join("test-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let stub_claude = bin_dir.join("claude");
    // Copy the stub so it's independent of the source file's perms.
    std::fs::copy(&stub_path, &stub_claude).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&stub_claude).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub_claude, perms).unwrap();
    }
    let augmented_path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    // `ryve hand spawn <spark_id> --role bug_hunter --agent claude`.
    let spawn = Command::new(ryve_bin())
        .env("RYVE_WORKSHOP_ROOT", &ws)
        .env("PATH", &augmented_path)
        // Make the spawn deterministic about actor (bypasses $USER).
        .env("RYVE_ACTOR_ID", "bughunter-test")
        .args([
            "--json",
            "hand",
            "spawn",
            &spark_id,
            "--role",
            "bug_hunter",
            "--agent",
            "claude",
        ])
        .current_dir(&ws)
        .output()
        .expect("spawn ryve hand spawn");

    assert!(
        spawn.status.success(),
        "bug hunter spawn MUST succeed under write-capable tool policy; \
         got stdout={} stderr={}",
        String::from_utf8_lossy(&spawn.stdout),
        String::from_utf8_lossy(&spawn.stderr)
    );

    let stdout = String::from_utf8_lossy(&spawn.stdout);
    let session_id =
        parse_json_string(&stdout, "session_id").expect("session_id in --json spawn output");
    let worktree_str =
        parse_json_string(&stdout, "worktree").expect("worktree in --json spawn output");
    let worktree_path = PathBuf::from(worktree_str);

    // Poll for the stub's outcome file. The stub runs inside tmux so
    // we cannot `wait` on it; file appearance means the process
    // actually executed. 5s is plenty — the stub records its
    // outcome before its sleep.
    let outcome_path = worktree_path.join("bug-hunter-smoke.txt");
    let deadline = Instant::now() + Duration::from_secs(5);
    let outcome = loop {
        if outcome_path.exists()
            && let Ok(s) = std::fs::read_to_string(&outcome_path)
            && !s.is_empty()
        {
            break s;
        }
        if Instant::now() >= deadline {
            panic!(
                "stub bug-hunter agent never wrote {} — spawn succeeded \
                 but the subprocess failed to run or its writes were \
                 rejected before it could record an outcome. worktree \
                 contents: {:?}",
                outcome_path.display(),
                std::fs::read_dir(&worktree_path).ok().map(|rd| rd
                    .filter_map(|e| e.ok().map(|e| e.file_name()))
                    .collect::<Vec<_>>())
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    // ─── (1) No write-denied errors — tool policy is write-capable. ──

    assert!(
        outcome.trim().starts_with("SMOKE_OK"),
        "bug hunter worktree must accept writes under the write-capable \
         tool policy; stub recorded: {outcome:?}"
    );

    // Belt-and-braces: both deliverables actually landed on disk.
    assert!(
        worktree_path.join("BUG_HUNTER_FIX.txt").exists(),
        "fix file must be written under write-capable policy"
    );
    assert!(
        worktree_path.join("tests/regression_test.txt").exists(),
        "regression test file must be written under write-capable policy"
    );

    // ─── (2) Session row carries the archetype's session_label. ─────

    let pool = data::db::open_sparks_db(&ws).await.expect("open sparks.db");
    let workshop_id = workshop_id_of(&ws);
    let sessions_db = agent_session_repo::list_for_workshop(&pool, &workshop_id)
        .await
        .expect("list sessions");
    let row = sessions_db
        .iter()
        .find(|s| s.id == session_id)
        .expect("session row for spawned bug hunter");
    assert_eq!(
        row.session_label.as_deref(),
        Some("bug_hunter"),
        "bug hunter spawn must persist session_label = 'bug_hunter'"
    );
    drop(pool);

    // Workshop cleanup. The tmux session self-terminates when the
    // stub's `sleep 3` exits; `remove_dir_all` collects residual
    // state.
    let _ = std::fs::remove_dir_all(&ws);
}
