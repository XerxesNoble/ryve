//! End-to-end integration test for the Releases lifecycle [sp-2a82fee7].
//!
//! Drives the full ceremony through the `ryve` CLI in a temp workshop:
//!   create release → branch exists → add two toy epics → close epics
//!   → release close → assert (a) tag exists on the release branch,
//!   (b) artifact file exists at the recorded path, (c) releases row is
//!   in closed state.
//!
//! The temp workshop is a self-contained git repo + fixture cargo project
//! named `ryve` so that `release close` can run a real release build
//! against it without ever touching the surrounding development worktree.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn ryve_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ryve"))
}

/// Build a fresh, isolated workshop:
///   - tempdir under the system tempdir,
///   - git repo with one initial commit on `main`,
///   - tiny `ryve` cargo fixture so `cargo build --release` (invoked by
///     `ryve release close` via [`crate::release_artifact`]) produces a
///     binary called `ryve`,
///   - `.gitignore` is whatever `fresh_workshop_with` is told to write under
///     the `[ryve_ignore]` slot — by default `".ryve"` (the broad exclusion
///     used by most release-flow tests so the working tree stays clean
///     after `ryve init`). The companion `fresh_workshop_tracked_ryve`
///     variant uses a narrower pattern that mirrors the real ryve repo,
///     leaving `.ryve/config.toml` + `.ryve/ui_state.json` tracked so the
///     dirty-tree allowlist gets exercised end-to-end. Either way,
///     `release_branch::cut_release_branch` and `tag_release` continue to
///     refuse on a dirty tree (now modulo the live-workspace allowlist).
fn fresh_workshop() -> PathBuf {
    fresh_workshop_with(".ryve")
}

/// Variant of [`fresh_workshop`] that leaves `.ryve/config.toml` and
/// `.ryve/ui_state.json` tracked in git (mirroring the real ryve repo).
/// Only `.ryve/sparks.db*` and runtime subdirs are ignored, so any live UI
/// rewrite of those two files shows up as a dirty diff — which is exactly
/// the scenario the dirty-tree allowlist is meant to handle.
fn fresh_workshop_tracked_ryve() -> PathBuf {
    let root = fresh_workshop_with(
        // Ignore only the runtime bits that can't be versioned; leave
        // config.toml and ui_state.json tracked.
        ".ryve/sparks.db\n.ryve/sparks.db-*\n.ryve/prompts/\n.ryve/logs/\n.ryve/worktrees/\n.ryve/backups/\n.ryve/releases/",
    );
    // Seed both live-workshop files with committed contents so the later
    // test can modify them and observe the diff.
    fs::write(root.join(".ryve").join("config.toml"), "# initial\n").unwrap();
    fs::write(
        root.join(".ryve").join("ui_state.json"),
        "{\"collapsed_epics\":[]}\n",
    )
    .unwrap();
    git(&root, &["add", ".ryve/config.toml", ".ryve/ui_state.json"]);
    git(&root, &["commit", "-m", "seed live ryve workspace files"]);
    root
}

/// Core workshop builder — `ryve_ignore` is the literal `.gitignore` snippet
/// used for `.ryve/` entries. The caller controls how much of `.ryve/` is
/// tracked vs. ignored.
fn fresh_workshop_with(ryve_ignore: &str) -> PathBuf {
    let mut root = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    root.push(format!("ryve-release-e2e-{nanos}-{}", std::process::id()));
    fs::create_dir_all(&root).expect("create tempdir");

    git(&root, &["init", "--initial-branch", "main"]);
    git(&root, &["config", "user.name", "test"]);
    git(&root, &["config", "user.email", "test@test.com"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    git(&root, &["config", "--local", "tag.gpgSign", "false"]);

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"ryve\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
         [[bin]]\nname = \"ryve\"\npath = \"src/main.rs\"\n\n\
         [profile.release]\nopt-level = 0\nlto = false\ncodegen-units = 256\nstrip = false\n",
    )
    .unwrap();
    fs::write(
        root.join("src").join("main.rs"),
        "fn main() { println!(\"ryve fixture\"); }\n",
    )
    .unwrap();
    fs::write(
        root.join(".gitignore"),
        format!("{ryve_ignore}\n/target/\nCargo.lock\n"),
    )
    .unwrap();

    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "init"]);

    let status = Command::new(ryve_bin())
        .arg("init")
        .current_dir(&root)
        .env("RYVE_WORKSHOP_ROOT", &root)
        .status()
        .expect("spawn ryve init");
    assert!(status.success(), "ryve init failed in {root:?}");

    // `ryve init` writes RYVE.md (and may write other onboarding files in
    // future); commit anything it produced so the working tree is clean
    // before `ryve release create`, which refuses dirty trees.
    git(&root, &["add", "."]);
    let porcelain_after_add = git_output(&root, &["status", "--porcelain"]);
    if !porcelain_after_add.is_empty() {
        git(&root, &["commit", "-m", "ryve init"]);
    }

    let porcelain = git_output(&root, &["status", "--porcelain"]);
    assert!(
        porcelain.is_empty(),
        "expected clean tree before release create, got: {porcelain:?}"
    );

    root
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "ryve-test")
        .env("GIT_AUTHOR_EMAIL", "ryve-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "ryve-test")
        .env("GIT_COMMITTER_EMAIL", "ryve-test@example.invalid")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed in {root:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn git_output(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn run(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ryve_bin())
        .args(args)
        .current_dir(root)
        .env("RYVE_WORKSHOP_ROOT", root)
        .output()
        .expect("spawn ryve")
}

fn run_assert(root: &Path, args: &[&str]) -> std::process::Output {
    let out = run(root, args);
    assert!(
        out.status.success(),
        "ryve {args:?} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out
}

fn create_epic(root: &Path, title: &str) -> String {
    let out = run_assert(
        root,
        &["--json", "spark", "create", "--type", "epic", title],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("epic create stdout was not JSON: {e}"));
    v["id"]
        .as_str()
        .expect("created spark JSON missing `id`")
        .to_string()
}

#[test]
fn release_e2e_create_through_close() {
    let ws = fresh_workshop();

    // 1. Create release with patch bump from the implicit 0.0.0 baseline.
    let out = run_assert(&ws, &["--json", "release", "create", "patch"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("release create stdout was not JSON: {e}"));
    let release_id = v["release"]["id"].as_str().unwrap().to_string();
    let version = v["release"]["version"].as_str().unwrap().to_string();
    let branch = v["branch"].as_str().unwrap().to_string();
    assert_eq!(version, "0.0.1", "patch from 0.0.0 should yield 0.0.1");
    assert_eq!(
        branch, "release/0.0.1",
        "branch should follow the release/<version> convention"
    );

    // 1b. The release branch must actually exist in the git repo, not just
    //     be recorded in the DB row.
    let branch_check = Command::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(&ws)
        .status()
        .expect("git show-ref");
    assert!(
        branch_check.success(),
        "release branch {branch} missing from git repo at {}",
        ws.display()
    );

    // 2. Create two toy epics and add them to the release.
    let epic1 = create_epic(&ws, "toy epic one");
    let epic2 = create_epic(&ws, "toy epic two");
    run_assert(&ws, &["release", "add-epic", &release_id, &epic1]);
    run_assert(&ws, &["release", "add-epic", &release_id, &epic2]);

    // 3. Close both epics — the close gate refuses any release whose
    //    member epics are not all in the closed state.
    run_assert(&ws, &["spark", "close", &epic1, "completed"]);
    run_assert(&ws, &["spark", "close", &epic2, "completed"]);

    // 4. Run the release-close ceremony. This checks out the release
    //    branch, tags it, builds the fixture artifact, records the path,
    //    and transitions the row to closed.
    let out = run_assert(&ws, &["--json", "release", "close", &release_id]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("release close stdout was not JSON: {e}"));
    let tag = v["tag"].as_str().unwrap().to_string();
    let artifact_path = v["artifact_path"].as_str().unwrap().to_string();
    assert_eq!(tag, "v0.0.1", "tag name must be `v<version>`");

    // (a) The tag exists and points at the same commit as the release
    //     branch HEAD — i.e. it tags the right tip.
    let tag_target = git_output(&ws, &["rev-list", "-n", "1", &tag]);
    let branch_head = git_output(&ws, &["rev-list", "-n", "1", &branch]);
    assert_eq!(
        tag_target, branch_head,
        "tag {tag} must point at release branch {branch} HEAD"
    );

    // (b) The artifact file exists at the recorded path and is non-empty.
    let p = Path::new(&artifact_path);
    assert!(p.exists(), "artifact file should exist at {artifact_path}");
    let md = fs::metadata(p).unwrap();
    assert!(md.len() > 0, "artifact file should be non-empty");

    // (c) The releases row is in the closed state with both metadata
    //     fields persisted.
    let show = run_assert(&ws, &["--json", "release", "show", &release_id]);
    let v: serde_json::Value = serde_json::from_slice(&show.stdout)
        .unwrap_or_else(|e| panic!("release show stdout was not JSON: {e}"));
    assert_eq!(
        v["release"]["status"].as_str(),
        Some("closed"),
        "release row must end in the closed state"
    );
    assert_eq!(
        v["release"]["tag"].as_str(),
        Some(tag.as_str()),
        "release row must persist the tag name"
    );
    assert_eq!(
        v["release"]["artifact_path"].as_str(),
        Some(artifact_path.as_str()),
        "release row must persist the artifact path"
    );
}

/// Regression for spark ryve-b3d4547d: `ryve release close` must succeed
/// when the only unstaged diffs are the Ryve-internal workspace files that
/// the live UI rewrites (`.ryve/config.toml`, `.ryve/ui_state.json`). Those
/// paths are allowlisted in the `release_branch` dirty-tree gate so a user
/// can close a release from the running UI without having to quit it first.
#[test]
fn release_e2e_close_tolerates_dirty_live_ryve_workspace_files() {
    let ws = fresh_workshop_tracked_ryve();

    // Create and populate the release exactly like the happy-path test.
    let out = run_assert(&ws, &["--json", "release", "create", "patch"]);
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("release create stdout JSON");
    let release_id = v["release"]["id"].as_str().unwrap().to_string();
    let version = v["release"]["version"].as_str().unwrap().to_string();

    let epic = create_epic(&ws, "dirty-workspace toy epic");
    run_assert(&ws, &["release", "add-epic", &release_id, &epic]);
    run_assert(&ws, &["spark", "close", &epic, "completed"]);

    // Simulate the live UI mid-session: rewrite both allowlisted files on
    // disk without committing. Before our fix this would have made
    // `tag_release` error with `DirtyWorkingTree`, aborting the close.
    fs::write(
        ws.join(".ryve").join("config.toml"),
        "# rewritten by live UI during close\n",
    )
    .unwrap();
    fs::write(
        ws.join(".ryve").join("ui_state.json"),
        "{\"collapsed_epics\":[\"ep-live\"]}\n",
    )
    .unwrap();

    // Sanity: git reports the tree as dirty (the scenario we're covering).
    let porcelain = git_output(&ws, &["status", "--porcelain"]);
    assert!(
        porcelain.contains(".ryve/config.toml"),
        "precondition: config.toml should appear dirty, got: {porcelain:?}"
    );
    assert!(
        porcelain.contains(".ryve/ui_state.json"),
        "precondition: ui_state.json should appear dirty, got: {porcelain:?}"
    );

    // Close the release — this must succeed despite the dirty allowlisted
    // files. Any other dirty file would still abort; we don't introduce
    // one here because that case is covered at the release_branch level.
    let out = run_assert(&ws, &["--json", "release", "close", &release_id]);
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("release close stdout JSON");
    let tag = v["tag"].as_str().unwrap().to_string();
    assert_eq!(tag, format!("v{version}"));

    // The tag and artifact must be in place, and the row must be closed.
    let tag_exists = Command::new("git")
        .args(["tag", "--list", &tag])
        .current_dir(&ws)
        .output()
        .expect("git tag list");
    assert_eq!(
        String::from_utf8_lossy(&tag_exists.stdout).trim(),
        tag,
        "release tag must exist after close"
    );
    let artifact_path = v["artifact_path"].as_str().unwrap();
    assert!(
        Path::new(artifact_path).exists(),
        "artifact {artifact_path} must exist after close"
    );

    let show = run_assert(&ws, &["--json", "release", "show", &release_id]);
    let v: serde_json::Value = serde_json::from_slice(&show.stdout).expect("release show JSON");
    assert_eq!(v["release"]["status"].as_str(), Some("closed"));
}
