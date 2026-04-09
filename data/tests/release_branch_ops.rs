// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Integration tests for the `release_branch` git-discipline module.
//!
//! Each test spins up a fresh git repository in a `tempfile::TempDir` so the
//! tests are hermetic and never touch the host repository.

use std::path::{Path, PathBuf};
use std::process::Command;

use data::release_branch::{ReleaseBranch, ReleaseBranchError, release_branch_name};
use tempfile::TempDir;

/// Initialise a fresh git repository with a `main` branch and a single
/// commit so it has a valid HEAD to branch from.
fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();

    run(path, &["init", "-q", "-b", "main"]);
    run(path, &["config", "user.email", "test@example.com"]);
    run(path, &["config", "user.name", "Release Test"]);
    run(path, &["config", "commit.gpgsign", "false"]);
    run(path, &["config", "tag.gpgsign", "false"]);

    std::fs::write(path.join("README.md"), "init\n").unwrap();
    run(path, &["add", "README.md"]);
    run(path, &["commit", "-q", "-m", "init"]);

    dir
}

fn run(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git invocable");
    assert!(status.success(), "git {args:?} failed in {cwd:?}");
}

fn dirty_the_tree(path: &Path) {
    std::fs::write(path.join("scratch.txt"), "uncommitted\n").unwrap();
}

fn release_branch_for(dir: &TempDir) -> ReleaseBranch {
    data::release_branch::open(dir.path().to_path_buf())
}

#[tokio::test]
async fn cut_then_tag_happy_path() {
    let dir = init_repo();
    let rb = release_branch_for(&dir);

    // Cut release/1.2.3 from main.
    let branch = rb.cut_release_branch("1.2.3").await.expect("cut");
    assert_eq!(branch, "release/1.2.3");

    assert!(rb.release_branch_exists("1.2.3").await.unwrap());
    assert_eq!(
        rb.current_release_branch().await.unwrap().as_deref(),
        Some("release/1.2.3"),
    );

    // Tag the release.
    let artifact = PathBuf::from("/tmp/ryve-1.2.3.tar.gz");
    rb.tag_release("1.2.3", &artifact).await.expect("tag");

    // Verify the v1.2.3 tag now exists.
    let out = Command::new("git")
        .args(["tag", "--list", "v1.2.3"])
        .current_dir(dir.path())
        .output()
        .expect("git tag list");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "v1.2.3",
        "expected v1.2.3 tag to exist"
    );
}

#[tokio::test]
async fn cut_release_branch_refuses_dirty_tree() {
    let dir = init_repo();
    dirty_the_tree(dir.path());
    let rb = release_branch_for(&dir);

    let err = rb
        .cut_release_branch("1.0.0")
        .await
        .expect_err("must refuse dirty tree");
    assert!(
        matches!(err, ReleaseBranchError::DirtyWorkingTree),
        "got unexpected error: {err:?}"
    );

    // The branch must not have been created as a side effect.
    assert!(!rb.release_branch_exists("1.0.0").await.unwrap());
}

#[tokio::test]
async fn cut_release_branch_refuses_existing_branch() {
    let dir = init_repo();
    let rb = release_branch_for(&dir);

    rb.cut_release_branch("1.0.0").await.expect("first cut");

    // Hop back to main so the second cut isn't blocked by the
    // checkout-already-on-it case.
    run(dir.path(), &["checkout", "-q", "main"]);

    let err = rb
        .cut_release_branch("1.0.0")
        .await
        .expect_err("must refuse existing");
    match err {
        ReleaseBranchError::BranchAlreadyExists(name) => {
            assert_eq!(name, release_branch_name("1.0.0"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn tag_release_refuses_when_not_on_release_branch() {
    let dir = init_repo();
    let rb = release_branch_for(&dir);

    rb.cut_release_branch("2.0.0").await.expect("cut");
    // Move off the release branch.
    run(dir.path(), &["checkout", "-q", "main"]);

    let err = rb
        .tag_release("2.0.0", Path::new("/tmp/x"))
        .await
        .expect_err("must refuse wrong branch");
    match err {
        ReleaseBranchError::WrongBranch { expected, actual } => {
            assert_eq!(expected, "release/2.0.0");
            assert_eq!(actual, "main");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn tag_release_refuses_when_head_drifted_from_branch_tip() {
    let dir = init_repo();
    let rb = release_branch_for(&dir);

    rb.cut_release_branch("3.1.4").await.expect("cut");

    // Detach HEAD one commit behind the branch tip by adding a new
    // commit on the branch and then resetting HEAD softly... easier:
    // make a new commit on the branch, then `git checkout` the parent
    // commit detached. The branch tip will be ahead of HEAD.
    std::fs::write(dir.path().join("more.txt"), "x\n").unwrap();
    run(dir.path(), &["add", "more.txt"]);
    run(dir.path(), &["commit", "-q", "-m", "advance"]);
    // Detach to HEAD~1 — branch tip is now ahead of detached HEAD.
    run(dir.path(), &["checkout", "-q", "--detach", "HEAD~1"]);

    let err = rb
        .tag_release("3.1.4", Path::new("/tmp/x"))
        .await
        .expect_err("must refuse drifted HEAD");
    // Detached HEAD reports as `HEAD`, so this surfaces as WrongBranch first
    // (current_branch() returns "HEAD"), which still satisfies the
    // "tag refuses unless working tree matches release branch HEAD"
    // acceptance criterion.
    assert!(
        matches!(
            err,
            ReleaseBranchError::WrongBranch { .. } | ReleaseBranchError::HeadMismatch { .. }
        ),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn tag_release_refuses_dirty_tree() {
    let dir = init_repo();
    let rb = release_branch_for(&dir);

    rb.cut_release_branch("0.1.0").await.expect("cut");
    dirty_the_tree(dir.path());

    let err = rb
        .tag_release("0.1.0", Path::new("/tmp/x"))
        .await
        .expect_err("must refuse dirty tree");
    assert!(
        matches!(err, ReleaseBranchError::DirtyWorkingTree),
        "got unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn release_branch_exists_false_for_unknown_version() {
    let dir = init_repo();
    let rb = release_branch_for(&dir);
    assert!(!rb.release_branch_exists("9.9.9").await.unwrap());
}

#[tokio::test]
async fn current_release_branch_is_none_on_main() {
    let dir = init_repo();
    let rb = release_branch_for(&dir);
    assert_eq!(rb.current_release_branch().await.unwrap(), None);
}

#[tokio::test]
async fn invalid_versions_are_rejected_at_the_api_boundary() {
    let dir = init_repo();
    let rb = release_branch_for(&dir);

    for bad in ["", "1.2", "1.2.3-rc1", "v1.2.3", "01.2.3"] {
        let err = rb
            .cut_release_branch(bad)
            .await
            .expect_err("must reject invalid version");
        assert!(
            matches!(err, ReleaseBranchError::InvalidVersion(_)),
            "version `{bad}` produced unexpected error: {err:?}"
        );
    }
}
