//! CLI integration tests for `ryve spark create` orphan rejection and the
//! `--parent` flag wired up by spark `ryve-c9d5a967`.
//!
//! These tests shell out to the built `ryve` binary against a fresh,
//! per-test workshop in a tempdir. They cover the four acceptance criteria
//! from the spark intent:
//!   1. non-epic spark with no --parent exits non-zero with a friendly hint
//!   2. --parent <epic> creates a child spark + parent_child bond
//!   3. --parent must point at an existing epic-typed spark
//!   4. --type epic still works with no --parent

use std::path::PathBuf;
use std::process::Command;

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// Make a fresh workshop dir under the system tempdir and run `ryve init`
/// in it. Returns the workshop root path. We pin RYVE_WORKSHOP_ROOT on
/// every subsequent invocation so the binary doesn't walk up into the
/// surrounding development worktree.
fn fresh_workshop() -> PathBuf {
    let mut root = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    root.push(format!("ryve-cli-test-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create tempdir");

    let status = Command::new(ryve_bin())
        .arg("init")
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .status()
        .expect("spawn ryve init");
    assert!(status.success(), "ryve init failed in {root:?}");
    root
}

fn run(root: &PathBuf, args: &[&str]) -> std::process::Output {
    Command::new(ryve_bin())
        .args(args)
        .current_dir(root)
        .env("RYVE_WORKSHOP_ROOT", root)
        .output()
        .expect("spawn ryve")
}

/// Extract a spark id (`ryve-xxxxxxxx`) from `created <id> — <title>` output.
fn parse_created_id(stdout: &str) -> String {
    stdout
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("could not parse created id from: {stdout:?}"))
        .to_string()
}

#[test]
fn task_without_parent_is_rejected_with_friendly_message() {
    let ws = fresh_workshop();
    let out = run(&ws, &["spark", "create", "--type", "task", "orphan attempt"]);
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--parent"),
        "stderr should name the --parent flag, got: {stderr}"
    );
    assert!(
        stderr.contains("--type epic"),
        "stderr should suggest --type epic, got: {stderr}"
    );
}

#[test]
fn epic_can_be_created_without_parent() {
    let ws = fresh_workshop();
    let out = run(&ws, &["spark", "create", "--type", "epic", "top-level epic"]);
    assert!(
        out.status.success(),
        "epic create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn task_with_epic_parent_creates_child_and_parent_child_bond() {
    let ws = fresh_workshop();
    let epic_out = run(&ws, &["spark", "create", "--type", "epic", "host epic"]);
    assert!(epic_out.status.success());
    let epic_id = parse_created_id(&String::from_utf8_lossy(&epic_out.stdout));

    let child_out = run(
        &ws,
        &[
            "spark", "create", "--type", "task", "--parent", &epic_id, "child task",
        ],
    );
    assert!(
        child_out.status.success(),
        "child create failed: {}",
        String::from_utf8_lossy(&child_out.stderr)
    );
    let child_id = parse_created_id(&String::from_utf8_lossy(&child_out.stdout));

    // The parent_id column on the row should point at the epic.
    let show = run(&ws, &["--json", "spark", "show", &child_id]);
    assert!(show.status.success());
    let show_stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        show_stdout.contains(&format!("\"parent_id\": \"{epic_id}\"")),
        "expected parent_id={epic_id} in show output: {show_stdout}"
    );

    // A parent_child bond should also exist (the dual representation in the
    // bond graph that the UI walks).
    let bonds = run(&ws, &["bond", "list", &child_id]);
    assert!(bonds.status.success());
    let bonds_stdout = String::from_utf8_lossy(&bonds.stdout);
    assert!(
        bonds_stdout.contains("parent_child") && bonds_stdout.contains(&epic_id),
        "expected parent_child bond from {epic_id} in: {bonds_stdout}"
    );
}

#[test]
fn parent_must_be_an_epic_not_some_other_type() {
    let ws = fresh_workshop();
    // Build a valid task under a real epic, then try to use that task as a
    // parent — this exercises the "exists but wrong type" branch.
    let epic_out = run(&ws, &["spark", "create", "--type", "epic", "the epic"]);
    let epic_id = parse_created_id(&String::from_utf8_lossy(&epic_out.stdout));

    let task_out = run(
        &ws,
        &[
            "spark", "create", "--type", "task", "--parent", &epic_id, "first task",
        ],
    );
    let task_id = parse_created_id(&String::from_utf8_lossy(&task_out.stdout));

    let bad = run(
        &ws,
        &[
            "spark",
            "create",
            "--type",
            "task",
            "--parent",
            &task_id,
            "task under task",
        ],
    );
    assert!(!bad.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(
        stderr.contains("only epics may be parents"),
        "stderr should explain the type rule, got: {stderr}"
    );
}

#[test]
fn parent_must_exist() {
    let ws = fresh_workshop();
    let out = run(
        &ws,
        &[
            "spark",
            "create",
            "--type",
            "task",
            "--parent",
            "ryve-deadbeef",
            "ghost child",
        ],
    );
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found"),
        "stderr should say not found, got: {stderr}"
    );
}
