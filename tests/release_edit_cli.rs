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

    Command::new("git")
        .args(["init", "--initial-branch", "main"])
        .current_dir(&root)
        .status()
        .expect("git init");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(&root)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .status()
        .expect("git commit");

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
