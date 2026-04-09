// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Disciplined git operations for Release branches.
//!
//! Every Release owns a dedicated `release/<version>` branch cut from `main`
//! at creation. This module is the **only** place in Ryve allowed to mutate
//! release branches: no other call site should invoke `git` against
//! `release/*` directly.
//!
//! Scope of this module (intentional non-goals):
//! - Merging feature branches into a release branch — owned by the Merge Hand.
//! - Cherry-picking individual commits — out of scope.

use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::git::{GitError, Repository};

/// Branch name prefix for every release. The full branch name is exactly
/// `release/<version>`.
pub const RELEASE_BRANCH_PREFIX: &str = "release/";

/// Build the canonical release branch name for a version.
pub fn release_branch_name(version: &str) -> String {
    format!("{RELEASE_BRANCH_PREFIX}{version}")
}

/// Errors raised by the release-branch module.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseBranchError {
    #[error("working tree is dirty; refusing to cut release branch")]
    DirtyWorkingTree,

    #[error("release branch already exists: {0}")]
    BranchAlreadyExists(String),

    #[error("release branch does not exist: {0}")]
    BranchNotFound(String),

    #[error(
        "working tree is not on the expected release branch (expected {expected}, found {actual})"
    )]
    WrongBranch { expected: String, actual: String },

    #[error("working tree HEAD ({head}) does not match release branch HEAD ({branch_head})")]
    HeadMismatch { head: String, branch_head: String },

    #[error("invalid release version `{0}`: expected strict semver MAJOR.MINOR.PATCH")]
    InvalidVersion(String),

    #[error("git command failed: {0}")]
    Command(String),

    #[error("git i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Git(#[from] GitError),
}

/// Disciplined release-branch operations bound to a single repository.
#[derive(Debug, Clone)]
pub struct ReleaseBranch {
    repo: Repository,
}

impl ReleaseBranch {
    /// Create a new release-branch handle for `repo`.
    pub fn new(repo: Repository) -> Self {
        Self { repo }
    }

    /// Path of the underlying repository.
    pub fn repo_path(&self) -> &Path {
        &self.repo.path
    }

    /// Cut a fresh release branch named `release/<version>` from `main`.
    ///
    /// Refuses with a typed error if:
    /// - the version is not strict `MAJOR.MINOR.PATCH` semver,
    /// - the working tree is dirty,
    /// - a `release/<version>` branch already exists.
    ///
    /// On success, returns the full branch name and leaves the working tree
    /// checked out on it.
    pub async fn cut_release_branch(&self, version: &str) -> Result<String, ReleaseBranchError> {
        validate_version(version)?;
        let branch = release_branch_name(version);

        if self.is_dirty().await? {
            return Err(ReleaseBranchError::DirtyWorkingTree);
        }
        if self.release_branch_exists(version).await? {
            return Err(ReleaseBranchError::BranchAlreadyExists(branch));
        }

        run_git(&self.repo.path, &["checkout", "-b", &branch, "main"]).await?;

        Ok(branch)
    }

    /// Tag the current `release/<version>` HEAD as `v<version>`.
    ///
    /// Refuses with a typed error unless the working tree is checked out
    /// on `release/<version>` AND `HEAD` resolves to the same commit as the
    /// branch tip. The `artifact_path` is recorded in the tag message so the
    /// tag carries a pointer back to the built artifact.
    pub async fn tag_release(
        &self,
        version: &str,
        artifact_path: &Path,
    ) -> Result<(), ReleaseBranchError> {
        validate_version(version)?;
        let branch = release_branch_name(version);

        if !self.release_branch_exists(version).await? {
            return Err(ReleaseBranchError::BranchNotFound(branch));
        }

        // Working tree must currently be on the release branch.
        let current = self.repo.current_branch().await?;
        if current != branch {
            return Err(ReleaseBranchError::WrongBranch {
                expected: branch,
                actual: current,
            });
        }

        // HEAD commit must equal the branch tip commit (no detached drift,
        // no uncommitted index, no rebase mid-flight).
        let head_sha = rev_parse(&self.repo.path, "HEAD").await?;
        let branch_sha = rev_parse(&self.repo.path, &branch).await?;
        if head_sha != branch_sha {
            return Err(ReleaseBranchError::HeadMismatch {
                head: head_sha,
                branch_head: branch_sha,
            });
        }

        // Working tree must also be clean — a tag against a dirty tree would
        // misrepresent what was released.
        if self.is_dirty().await? {
            return Err(ReleaseBranchError::DirtyWorkingTree);
        }

        let tag = format!("v{version}");
        let message = format!("Release {version} (artifact: {})", artifact_path.display());
        run_git(&self.repo.path, &["tag", "-a", &tag, "-m", &message]).await?;

        Ok(())
    }

    /// Returns `true` if a local `release/<version>` branch exists.
    pub async fn release_branch_exists(&self, version: &str) -> Result<bool, ReleaseBranchError> {
        validate_version(version)?;
        let branch = release_branch_name(version);
        let refname = format!("refs/heads/{branch}");
        let output = Command::new("git")
            .args(["show-ref", "--verify", "--quiet", &refname])
            .current_dir(&self.repo.path)
            .output()
            .await?;
        Ok(output.status.success())
    }

    /// If the working tree is currently on a `release/*` branch, return its
    /// full name. Otherwise return `None` (including detached-HEAD state).
    pub async fn current_release_branch(&self) -> Result<Option<String>, ReleaseBranchError> {
        let branch = self.repo.current_branch().await?;
        if branch.starts_with(RELEASE_BRANCH_PREFIX) {
            Ok(Some(branch))
        } else {
            Ok(None)
        }
    }

    async fn is_dirty(&self) -> Result<bool, ReleaseBranchError> {
        let output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.repo.path)
            .output()
            .await?;
        if !output.status.success() {
            return Err(ReleaseBranchError::Command(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(!output.stdout.is_empty())
    }
}

async fn run_git(repo: &Path, args: &[&str]) -> Result<(), ReleaseBranchError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .await?;
    if !output.status.success() {
        return Err(ReleaseBranchError::Command(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(())
}

async fn rev_parse(repo: &Path, refname: &str) -> Result<String, ReleaseBranchError> {
    let output = Command::new("git")
        .args(["rev-parse", refname])
        .current_dir(repo)
        .output()
        .await?;
    if !output.status.success() {
        return Err(ReleaseBranchError::Command(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Validate a release version as strict `MAJOR.MINOR.PATCH` semver.
///
/// Pre-release tags and build metadata are intentionally rejected at this
/// layer — they are non-goals of the v1 Releases epic and would silently
/// produce branches like `release/1.2.3-rc1` that the rest of the system
/// is not yet prepared to reason about.
fn validate_version(version: &str) -> Result<(), ReleaseBranchError> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return Err(ReleaseBranchError::InvalidVersion(version.to_string()));
    }
    for p in parts {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            return Err(ReleaseBranchError::InvalidVersion(version.to_string()));
        }
        // Reject leading zeros (e.g. "01") to keep versions canonical.
        if p.len() > 1 && p.starts_with('0') {
            return Err(ReleaseBranchError::InvalidVersion(version.to_string()));
        }
    }
    Ok(())
}

/// Convenience: build a `ReleaseBranch` for an arbitrary path.
pub fn open(path: impl Into<PathBuf>) -> ReleaseBranch {
    ReleaseBranch::new(Repository::new(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_branch_name_is_exactly_prefixed() {
        assert_eq!(release_branch_name("1.2.3"), "release/1.2.3");
    }

    #[test]
    fn validate_version_accepts_strict_semver() {
        assert!(validate_version("0.0.1").is_ok());
        assert!(validate_version("10.20.30").is_ok());
    }

    #[test]
    fn validate_version_rejects_non_semver() {
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "v1.2.3",
            "1.2.3-rc1",
            "1.2.3+build",
            "01.2.3",
        ] {
            assert!(
                validate_version(bad).is_err(),
                "expected `{bad}` to be rejected"
            );
        }
    }
}
