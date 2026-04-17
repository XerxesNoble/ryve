// SPDX-License-Identifier: AGPL-3.0-or-later

//! Watch duplicate-suppression end-to-end test
//! (spark ryve-638c69fd [sp-ee3f5c74]).
//!
//! The watches table has a partial unique index on
//! `(target_spark_id, intent_label)` covering non-cancelled rows. Atlas
//! depends on this contract to stay idempotent: when a coordination
//! flow reinstalls a watch on every wake, the second call must *fail
//! loudly* rather than silently stacking a duplicate that then
//! double-fires every slot.
//!
//! This test exercises the full CLI path — exit code, stderr message,
//! and the canonical "only one row in `watches`" assertion — because
//! that is the surface Atlas actually drives. Unit tests in
//! `data/src/sparks/watch_repo.rs` cover the repo-level error type;
//! this file pins the user-facing contract.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

// Atomic counter guarantees a unique tempdir per call even when Cargo runs
// tests in parallel — time-based naming alone can collide at the same
// nanosecond on fast machines.
static TEMPDIR_SEQ: AtomicU64 = AtomicU64::new(0);

fn fresh_workshop() -> PathBuf {
    let mut root = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = TEMPDIR_SEQ.fetch_add(1, Ordering::SeqCst);
    root.push(format!(
        "ryve-watch-dup-{nanos}-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create tempdir");

    // `ryve init` expects a workshop root with a git repo — a minimal
    // empty commit keeps the path resolution happy without bringing in
    // extra state.
    let git_init = Command::new("git")
        .args(["init", "--initial-branch", "main"])
        .current_dir(&root)
        .output()
        .expect("spawn git init");
    assert!(git_init.status.success(), "git init failed");
    let git_config = Command::new("git")
        .args(["config", "commit.gpgsign", "false"])
        .current_dir(&root)
        .output()
        .expect("spawn git config");
    assert!(
        git_config.status.success(),
        "git config commit.gpgsign failed"
    );
    let git_commit = Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(&root)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .expect("spawn git commit");
    assert!(git_commit.status.success(), "git commit failed");

    let status = Command::new(ryve_bin())
        .arg("init")
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .status()
        .expect("spawn ryve init");
    assert!(status.success(), "ryve init failed in {root:?}");
    root
}

fn run(root: &PathBuf, args: &[&str]) -> Output {
    Command::new(ryve_bin())
        .args(args)
        .current_dir(root)
        .env("RYVE_WORKSHOP_ROOT", root)
        .output()
        .expect("spawn ryve")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn assert_ok(out: &Output, ctx: &str) {
    assert!(
        out.status.success(),
        "{ctx} failed (exit {:?})\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// The core acceptance criterion for spark ryve-638c69fd: a second
/// `ryve watch create` on the same `(target, intent)` pair must exit
/// non-zero, name the existing watch id on stderr, and leave exactly
/// one row in the `watches` table — not two, not zero.
#[test]
fn duplicate_watch_create_exits_nonzero_and_preserves_single_row() {
    let ws = fresh_workshop();

    // First create: baseline — prints the new watch id on stdout.
    let first = run(
        &ws,
        &[
            "watch",
            "create",
            "ryve-dup-target",
            "--cadence",
            "60",
            "--intent",
            "release-monitor",
        ],
    );
    assert_ok(&first, "first watch create");
    let existing_id = stdout_of(&first);
    assert!(
        existing_id.starts_with("watch-"),
        "first create must print the new watch id on stdout; got {existing_id:?}"
    );

    // Second create on the same (target, intent): must fail loudly.
    let second = run(
        &ws,
        &[
            "watch",
            "create",
            "ryve-dup-target",
            "--cadence",
            "120",
            "--intent",
            "release-monitor",
        ],
    );
    assert!(
        !second.status.success(),
        "duplicate (target, intent) must exit non-zero; got success\nstdout:\n{}\nstderr:\n{}",
        stdout_of(&second),
        stderr_of(&second),
    );

    let stderr = stderr_of(&second);
    assert!(
        stderr.contains(&format!("watch already exists: {existing_id}")),
        "stderr must name the conflicting watch id verbatim; got: {stderr}"
    );
    assert!(
        stdout_of(&second).is_empty(),
        "duplicate create must not print a second watch id to stdout; got: {}",
        stdout_of(&second)
    );

    // Only one row exists for this (target, intent) — the partial unique
    // index did its job. We read through `watch list --json` rather than
    // touching sqlite directly (same discipline Atlas uses).
    let list = run(
        &ws,
        &["watch", "list", "--target", "ryve-dup-target", "--json"],
    );
    assert_ok(&list, "watch list --json after duplicate attempt");
    let rows: serde_json::Value =
        serde_json::from_str(&stdout_of(&list)).expect("watch list --json must return valid JSON");
    let arr = rows.as_array().expect("list output must be a JSON array");
    assert_eq!(
        arr.len(),
        1,
        "duplicate create must not insert a second row; got {arr:?}"
    );
    assert_eq!(
        arr[0]["id"].as_str(),
        Some(existing_id.as_str()),
        "the surviving row must be the original watch"
    );
    assert_eq!(
        arr[0]["intent_label"].as_str(),
        Some("release-monitor"),
        "the surviving intent label must be unchanged"
    );
    assert_eq!(
        arr[0]["cadence"].as_str(),
        Some("interval-secs:60"),
        "cadence must be the ORIGINAL 60s, not the rejected 120s"
    );
    assert_eq!(
        arr[0]["status"].as_str(),
        Some("active"),
        "surviving row must still be active"
    );
}

/// Different intent on the same target is legal and creates a second row.
/// Atlas depends on this to run multiple concurrent coordination flows
/// against the same spark (e.g. `release-monitor` + `pr-follow-through`).
#[test]
fn distinct_intents_on_same_target_coexist() {
    let ws = fresh_workshop();

    let a = run(
        &ws,
        &[
            "watch",
            "create",
            "ryve-dual-intent",
            "--cadence",
            "60",
            "--intent",
            "release-monitor",
        ],
    );
    assert_ok(&a, "first intent create");
    let id_a = stdout_of(&a);

    let b = run(
        &ws,
        &[
            "watch",
            "create",
            "ryve-dual-intent",
            "--cadence",
            "60",
            "--intent",
            "pr-follow-through",
        ],
    );
    assert_ok(
        &b,
        "second intent on same target must be allowed — dedup is per (target, intent)",
    );
    let id_b = stdout_of(&b);
    assert_ne!(
        id_a, id_b,
        "distinct intents must produce distinct watch ids"
    );

    let list = run(
        &ws,
        &["watch", "list", "--target", "ryve-dual-intent", "--json"],
    );
    assert_ok(&list, "watch list --json");
    let rows: serde_json::Value = serde_json::from_str(&stdout_of(&list)).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(
        arr.len(),
        2,
        "two watches on the same target with different intents must coexist"
    );
}

/// Cancelling a watch frees the `(target, intent)` slot — the partial
/// unique index excludes `cancelled` rows, so re-creating after cancel
/// succeeds. This is how Atlas reopens a coordination flow after the
/// previous one wrapped up early or was manually cancelled.
#[test]
fn cancelled_watch_frees_target_intent_slot() {
    let ws = fresh_workshop();

    let first = run(
        &ws,
        &[
            "watch",
            "create",
            "ryve-reopen",
            "--cadence",
            "60",
            "--intent",
            "release-monitor",
        ],
    );
    assert_ok(&first, "first create");
    let id_first = stdout_of(&first);

    // Cancel frees the slot.
    let cancel = run(&ws, &["watch", "cancel", &id_first]);
    assert_ok(&cancel, "cancel first watch");

    // Re-creating now succeeds and mints a new id.
    let second = run(
        &ws,
        &[
            "watch",
            "create",
            "ryve-reopen",
            "--cadence",
            "120",
            "--intent",
            "release-monitor",
        ],
    );
    assert_ok(
        &second,
        "re-create after cancel must succeed — partial index excludes cancelled rows",
    );
    let id_second = stdout_of(&second);
    assert_ne!(id_first, id_second);

    // The old row still exists (cancel is soft); history is preserved.
    let all = run(&ws, &["watch", "list", "--target", "ryve-reopen", "--json"]);
    assert_ok(&all, "watch list --json (both rows)");
    let rows: serde_json::Value = serde_json::from_str(&stdout_of(&all)).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(
        arr.len(),
        2,
        "cancelled + active rows must both persist for audit history"
    );
}
