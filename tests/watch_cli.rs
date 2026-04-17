// End-to-end CLI tests for `ryve watch` [sp-ee3f5c74]. Each test boots a
// throwaway workshop with `ryve init`, drives the five subcommands, and
// asserts stdout + exit codes against the acceptance criteria on
// spark ryve-ff8c2fda.

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
        "ryve-watch-cli-{nanos}-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create tempdir");

    // ryve init expects to find (or create) a workshop root; a minimal git
    // repo keeps the path resolution happy without bringing in extra state.
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

fn create_watch(root: &PathBuf, target: &str, intent: &str, cadence: &str) -> String {
    let out = run(
        root,
        &[
            "watch",
            "create",
            target,
            "--cadence",
            cadence,
            "--intent",
            intent,
        ],
    );
    assert_ok(&out, "watch create");
    let id = stdout_of(&out);
    assert!(
        id.starts_with("watch-"),
        "expected watch id on stdout, got: {id:?}"
    );
    id
}

#[test]
fn watch_create_prints_new_id_and_is_scriptable() {
    let ws = fresh_workshop();
    let id = create_watch(&ws, "ryve-target", "poll-status", "60");
    assert!(id.starts_with("watch-"));
}

#[test]
fn watch_list_and_filters_behave() {
    let ws = fresh_workshop();
    let _a = create_watch(&ws, "ryve-a", "intent-x", "30");
    let b = create_watch(&ws, "ryve-b", "intent-x", "30");

    let all = run(&ws, &["watch", "list"]);
    assert_ok(&all, "watch list");
    let stdout = stdout_of(&all);
    assert!(stdout.contains("ryve-a"), "expected ryve-a row: {stdout}");
    assert!(stdout.contains("ryve-b"), "expected ryve-b row: {stdout}");

    let filtered = run(&ws, &["watch", "list", "--target", "ryve-b"]);
    assert_ok(&filtered, "watch list --target");
    let filtered_stdout = stdout_of(&filtered);
    assert!(
        filtered_stdout.contains(&b) && !filtered_stdout.contains("ryve-a"),
        "--target should scope to ryve-b only, got: {filtered_stdout}"
    );

    let json = run(&ws, &["watch", "list", "--json"]);
    assert_ok(&json, "watch list --json");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout_of(&json)).expect("valid JSON from --json");
    assert!(parsed.is_array(), "expected JSON array");
    assert_eq!(parsed.as_array().unwrap().len(), 2);
}

#[test]
fn watch_show_renders_all_fields_and_json_mode() {
    let ws = fresh_workshop();
    let id = create_watch(&ws, "ryve-show", "intent-show", "120");

    let human = run(&ws, &["watch", "show", &id]);
    assert_ok(&human, "watch show");
    let text = stdout_of(&human);
    for needle in [
        "ID:",
        "Target:",
        "Cadence:",
        "Stop condition:",
        "Intent:",
        "Status:",
        "Last fired:",
        "Next fire:",
    ] {
        assert!(
            text.contains(needle),
            "watch show missing '{needle}': {text}"
        );
    }
    assert!(text.contains(&id));
    assert!(text.contains("ryve-show"));
    assert!(text.contains("intent-show"));

    let json = run(&ws, &["watch", "show", &id, "--json"]);
    assert_ok(&json, "watch show --json");
    let parsed: serde_json::Value = serde_json::from_str(&stdout_of(&json)).expect("valid JSON");
    assert_eq!(parsed["id"].as_str(), Some(id.as_str()));
    assert_eq!(parsed["target_spark_id"].as_str(), Some("ryve-show"));
    assert_eq!(parsed["intent_label"].as_str(), Some("intent-show"));
    assert_eq!(parsed["status"].as_str(), Some("active"));
}

#[test]
fn watch_cancel_marks_cancelled() {
    let ws = fresh_workshop();
    let id = create_watch(&ws, "ryve-cancel", "i", "45");
    let out = run(&ws, &["watch", "cancel", &id]);
    assert_ok(&out, "watch cancel");
    assert!(
        stdout_of(&out).contains(&id),
        "cancel output should mention id"
    );

    let show = run(&ws, &["watch", "show", &id, "--json"]);
    assert_ok(&show, "watch show after cancel");
    let parsed: serde_json::Value = serde_json::from_str(&stdout_of(&show)).unwrap();
    assert_eq!(parsed["status"].as_str(), Some("cancelled"));
}

#[test]
fn watch_replace_cancels_old_and_returns_new_id() {
    let ws = fresh_workshop();
    let original = create_watch(&ws, "ryve-replace", "intent", "60");

    let out = run(&ws, &["watch", "replace", &original, "--cadence", "300"]);
    assert_ok(&out, "watch replace");
    let new_id = stdout_of(&out);
    assert!(new_id.starts_with("watch-"));
    assert_ne!(new_id, original, "replace must mint a new watch id");

    // Old row must be cancelled.
    let old = run(&ws, &["watch", "show", &original, "--json"]);
    assert_ok(&old, "watch show old");
    let old_parsed: serde_json::Value = serde_json::from_str(&stdout_of(&old)).unwrap();
    assert_eq!(old_parsed["status"].as_str(), Some("cancelled"));

    // New row carries the new cadence + active status, intent inherited.
    let fresh = run(&ws, &["watch", "show", &new_id, "--json"]);
    assert_ok(&fresh, "watch show new");
    let new_parsed: serde_json::Value = serde_json::from_str(&stdout_of(&fresh)).unwrap();
    assert_eq!(new_parsed["status"].as_str(), Some("active"));
    assert_eq!(new_parsed["intent_label"].as_str(), Some("intent"));
    assert_eq!(
        new_parsed["cadence"].as_str(),
        Some("interval-secs:300"),
        "cadence should update",
    );
}

#[test]
fn watch_create_rejects_duplicate_target_intent() {
    let ws = fresh_workshop();
    let existing = create_watch(&ws, "ryve-dup", "the-intent", "60");

    let out = run(
        &ws,
        &[
            "watch",
            "create",
            "ryve-dup",
            "--cadence",
            "90",
            "--intent",
            "the-intent",
        ],
    );
    assert!(
        !out.status.success(),
        "duplicate (target, intent) must exit non-zero; got success"
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains(&format!("watch already exists: {existing}")),
        "stderr should name the conflicting watch id; got: {stderr}"
    );
}
