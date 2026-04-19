// CLI integration tests for `ryve release edit` [sp-ryve-2b1a37a8].

use std::path::PathBuf;
use std::process::Command;

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

fn fresh_workshop() -> PathBuf {
    let mut root = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    root.push(format!("ryve-cli-test-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create tempdir");

    let git_init = Command::new("git")
        .args(["init", "--initial-branch", "main"])
        .current_dir(&root)
        .output()
        .expect("spawn git init");
    assert!(
        git_init.status.success(),
        "git init failed in {root:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&git_init.stdout),
        String::from_utf8_lossy(&git_init.stderr)
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
    assert!(
        git_commit.status.success(),
        "git commit failed in {root:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&git_commit.stdout),
        String::from_utf8_lossy(&git_commit.stderr)
    );

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

fn create_release(root: &PathBuf) -> String {
    let out = run(root, &["release", "create", "major"]);
    assert!(
        out.status.success(),
        "release create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("could not parse release id from: {stdout}"))
        .to_string()
}

#[test]
fn release_edit_version_succeeds() {
    let ws = fresh_workshop();
    let id = create_release(&ws);

    let out = run(&ws, &["release", "edit", &id, "--version", "2.0.0"]);
    assert!(
        out.status.success(),
        "release edit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("v2.0.0"),
        "expected updated version in output, got: {stdout}"
    );
}

#[test]
fn release_edit_invalid_version_fails() {
    let ws = fresh_workshop();
    let id = create_release(&ws);

    let out = run(&ws, &["release", "edit", &id, "--version", "not-semver"]);
    assert!(
        !out.status.success(),
        "expected non-zero exit for invalid semver"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid semver"),
        "expected semver error, got: {stderr}"
    );
}

#[test]
fn release_edit_branch_round_trip() {
    let ws = fresh_workshop();
    let id = create_release(&ws);

    let out = run(&ws, &["release", "edit", &id, "--branch", "release/9.9.9"]);
    assert!(
        out.status.success(),
        "release edit --branch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let show = run(&ws, &["--json", "release", "show", &id]);
    assert!(
        show.status.success(),
        "release show failed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        stdout.contains("release/9.9.9"),
        "expected branch_name to round-trip via release show, got: {stdout}"
    );

    let clear = run(&ws, &["release", "edit", &id, "--clear-branch"]);
    assert!(
        clear.status.success(),
        "release edit --clear-branch failed: {}",
        String::from_utf8_lossy(&clear.stderr)
    );

    let show2 = run(&ws, &["--json", "release", "show", &id]);
    assert!(show2.status.success());
    let stdout2 = String::from_utf8_lossy(&show2.stdout);
    assert!(
        stdout2.contains("\"branch_name\": null"),
        "expected branch_name to be null after --clear-branch, got: {stdout2}"
    );
}

#[test]
fn release_edit_branch_requires_value() {
    let ws = fresh_workshop();
    let id = create_release(&ws);

    let out = run(&ws, &["release", "edit", &id, "--branch"]);
    assert!(
        !out.status.success(),
        "expected --branch without value to fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--branch"),
        "expected stderr to mention --branch, got: {stderr}"
    );
}
