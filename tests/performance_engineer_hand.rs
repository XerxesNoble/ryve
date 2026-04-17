// SPDX-License-Identifier: AGPL-3.0-or-later

//! Smoke spawn integration test for the Performance Engineer Hand
//! archetype [sp-1471f46a] / spark ryve-1c099466.
//!
//! Acceptance criterion from the spark:
//!
//! > Smoke test: archetype spawn against a toy workload produces at
//! > least one comment with before/after numbers.
//!
//! Performance Engineer is write-capable; its acceptance bar is a
//! measured delta vs a baseline (not a test pass), and the recording
//! surface for the before/after numbers is a `ryve comment add` on the
//! spark — post-mortems diff those comments. This test drives the real
//! `ryve hand spawn --role performance_engineer` CLI path against a
//! stub agent that simulates a Performance Engineer's core behaviours:
//!
//!   1. Lands a "fix" file in the worktree (write-capable policy).
//!   2. Posts a comment on the perf spark carrying `baseline → post-fix`
//!      numbers using the real `ryve comment add` subcommand.
//!
//! It then asserts that:
//!
//!   - `ryve hand spawn --role performance_engineer` succeeds (exit 0,
//!     no tool-policy error).
//!   - The stub's write lands on disk in the new worktree (tool
//!     policy resolved to write-capable).
//!   - The persisted `agent_sessions` row carries
//!     `session_label = "performance_engineer"`.
//!   - At least one comment exists on the perf spark AND its body
//!     contains before/after numbers (baseline + post-fix), matching
//!     the spark's acceptance criterion.
//!
//! The stub is intentionally minimal — the Performance Engineer's
//! tool / profiler / language choices are out of scope at the
//! archetype layer. What we verify here is the spawn-path plumbing
//! plus the comment-as-recording-surface invariant.
//!
//! Gated on the bundled tmux binary being available (same as the
//! `src/hand_spawn.rs` integration tests) — CI jobs without it skip.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use data::sparks::{agent_session_repo, comment_repo};

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
/// Mirrors `tests/bug_hunter_hand.rs::fresh_workshop`.
fn fresh_workshop() -> PathBuf {
    let uuid = uuid::Uuid::new_v4();
    let root =
        std::env::temp_dir().join(format!("ryve-perf-eng-test-{}-{uuid}", std::process::id()));
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

/// Stub agent that simulates a Performance Engineer's core flow
/// against a toy workload.
///
///   1. Emits a "fix" file (symbolic "the targeted change").
///   2. Invokes the real `ryve comment add <spark_id> '<baseline →
///      post-fix>'` subcommand so the recording-surface invariant is
///      exercised end-to-end (rather than mocked).
///   3. Writes a marker file so the harness can detect completion
///      without racing tmux attach.
///
/// The stub receives the perf spark id via the `RYVE_PERF_SMOKE_SPARK`
/// env var we set on the spawning `ryve hand spawn` — it's forwarded
/// into the subprocess env alongside the agent.
fn write_smoke_stub(workshop_dir: &Path, ryve_bin_path: &Path) -> PathBuf {
    let stub_path = workshop_dir.join("stub-perf-engineer-agent.sh");
    std::fs::write(
        &stub_path,
        format!(
            "#!/bin/sh\n\
             set -u\n\
             failures=\"\"\n\
             \n\
             # 1. Write the symbolic 'fix' file.\n\
             if ! echo 'performance engineer fix' > \"$PWD/PERF_FIX.txt\" 2>/dev/null; then\n\
                 failures=\"${{failures}}fix-write-denied \"\n\
             fi\n\
             \n\
             # 2. Post the before/after comment via the real ryve CLI.\n\
             #    The spark id flows in via RYVE_PERF_SMOKE_SPARK so the\n\
             #    stub is self-contained; the harness exports it before\n\
             #    `ryve hand spawn`.\n\
             if ! \"{ryve}\" comment add \"$RYVE_PERF_SMOKE_SPARK\" \\\n\
                 'perf smoke: baseline 42.0ms → post-fix 7.1ms (stub workload, --bench toy)' \\\n\
                 > \"$PWD/comment.stdout\" 2> \"$PWD/comment.stderr\"; then\n\
                 failures=\"${{failures}}comment-add-failed \"\n\
             fi\n\
             \n\
             # 3. Record outcome marker.\n\
             if [ -n \"$failures\" ]; then\n\
                 echo \"SMOKE_FAILED: $failures\" > \"$PWD/perf-engineer-smoke.txt\"\n\
             else\n\
                 echo \"SMOKE_OK\" > \"$PWD/perf-engineer-smoke.txt\"\n\
             fi\n\
             \n\
             # Keep the pane alive for tmux pipe-pane.\n\
             sleep 3\n",
            ryve = ryve_bin_path.display(),
        ),
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

/// Extract a JSON `"<key>"` string value from a flat JSON blob. Same
/// helper shape as `tests/bug_hunter_hand.rs::parse_json_string`.
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

/// Primary acceptance test for ryve-1c099466: a Performance Engineer
/// spawn against a toy workload must produce at least one comment on
/// the perf spark whose body carries before/after numbers.
#[tokio::test]
async fn performance_engineer_smoke_spawn_records_before_after_comment() {
    if !bundled_tmux_available() {
        eprintln!(
            "bundled tmux not available — skipping performance engineer \
             smoke test (production spawn path is hardened against the \
             pinned bundled tmux)"
        );
        return;
    }

    let ws = fresh_workshop();
    let stub_path = write_smoke_stub(&ws, &ryve_bin());

    // Create the synthetic perf epic the Performance Engineer will own.
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
            "toy workload: render_frame p99 regressed; recover baseline",
            "perf: synthetic toy workload for smoke spawn",
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

    // Register the stub under the `claude` agent name on a local PATH.
    // Matches the pattern used in `tests/bug_hunter_hand.rs`.
    let bin_dir = ws.join("test-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let stub_claude = bin_dir.join("claude");
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

    // `ryve hand spawn <spark_id> --role performance_engineer --agent
    // claude`. Forward the spark id into the subprocess env so the stub
    // can target it without parsing flags.
    let spawn = Command::new(ryve_bin())
        .env("RYVE_WORKSHOP_ROOT", &ws)
        .env("PATH", &augmented_path)
        .env("RYVE_ACTOR_ID", "perfeng-test")
        .env("RYVE_PERF_SMOKE_SPARK", &spark_id)
        .args([
            "--json",
            "hand",
            "spawn",
            &spark_id,
            "--role",
            "performance_engineer",
            "--agent",
            "claude",
        ])
        .current_dir(&ws)
        .output()
        .expect("spawn ryve hand spawn");

    assert!(
        spawn.status.success(),
        "performance engineer spawn MUST succeed under write-capable \
         tool policy; got stdout={} stderr={}",
        String::from_utf8_lossy(&spawn.stdout),
        String::from_utf8_lossy(&spawn.stderr)
    );

    let stdout = String::from_utf8_lossy(&spawn.stdout);
    let session_id =
        parse_json_string(&stdout, "session_id").expect("session_id in --json spawn output");
    let worktree_str =
        parse_json_string(&stdout, "worktree").expect("worktree in --json spawn output");
    let worktree_path = PathBuf::from(worktree_str);

    // Poll for the stub's outcome file. Matches the pattern used in
    // `tests/bug_hunter_hand.rs`: the stub runs inside tmux so we
    // cannot `wait` on it; file appearance means the process ran.
    let outcome_path = worktree_path.join("perf-engineer-smoke.txt");
    let deadline = Instant::now() + Duration::from_secs(10);
    let outcome = loop {
        if outcome_path.exists()
            && let Ok(s) = std::fs::read_to_string(&outcome_path)
            && !s.is_empty()
        {
            break s;
        }
        if Instant::now() >= deadline {
            panic!(
                "stub perf-engineer agent never wrote {} — spawn \
                 succeeded but the subprocess failed to run or its \
                 writes were rejected. worktree contents: {:?}",
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
        "performance engineer worktree must accept writes under the \
         write-capable tool policy AND the stub must have posted its \
         comment successfully; stub recorded: {outcome:?}"
    );
    assert!(
        worktree_path.join("PERF_FIX.txt").exists(),
        "symbolic fix file must be written under write-capable policy"
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
        .expect("session row for spawned performance engineer");
    assert_eq!(
        row.session_label.as_deref(),
        Some("performance_engineer"),
        "performance engineer spawn must persist \
         session_label = 'performance_engineer'"
    );

    // ─── (3) Acceptance criterion: at least one comment carrying
    //         before/after numbers is recorded on the perf spark. ────

    let comments = comment_repo::list_for_spark(&pool, &spark_id)
        .await
        .expect("list comments for perf spark");
    assert!(
        !comments.is_empty(),
        "acceptance criterion: performance engineer smoke spawn must \
         produce at least one comment on the perf spark; got 0"
    );
    // The stub's comment body must mechanically contain both the
    // baseline and the post-fix numbers. We match on the numeric
    // pattern the stub emits — if the stub shape changes, update both
    // sides of this assertion together.
    let perf_comment = comments
        .iter()
        .find(|c| c.body.contains("baseline") && c.body.contains("post-fix"))
        .unwrap_or_else(|| {
            panic!(
                "acceptance criterion: at least one comment must carry \
                 before/after numbers (baseline + post-fix). got: {:?}",
                comments.iter().map(|c| &c.body).collect::<Vec<_>>()
            )
        });
    assert!(
        perf_comment.body.contains("42.0ms") && perf_comment.body.contains("7.1ms"),
        "perf comment must carry both numeric values; body={:?}",
        perf_comment.body
    );

    drop(pool);

    // Workshop cleanup.
    let _ = std::fs::remove_dir_all(&ws);
}
