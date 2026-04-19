// SPDX-License-Identifier: AGPL-3.0-or-later

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

/// Paths the Ryve UI rewrites during normal operation. The dirty-tree gate
/// on `cut_release_branch` / `tag_release` skips these so a user can close a
/// release while the workshop is still live: a chevron click that updates
/// `.ryve/ui_state.json`, or a sidebar-width tweak persisted to
/// `.ryve/config.toml`, must not be able to break the release ceremony.
///
/// Keep this list narrow. Every entry here is a surface where the file's
/// current contents are *not* reflected in the tagged commit; widening it
/// risks shipping a release that doesn't match what's on disk.
pub(crate) const LIVE_WORKSPACE_FILES: &[&str] = &[".ryve/config.toml", ".ryve/ui_state.json"];

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
            .args(["status", "--porcelain=v1", "-z"])
            .current_dir(&self.repo.path)
            .output()
            .await?;
        if !output.status.success() {
            return Err(ReleaseBranchError::Command(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(porcelain_has_non_allowlisted_entry(
            &output.stdout,
            LIVE_WORKSPACE_FILES,
        ))
    }
}

/// Parse NUL-delimited `git status --porcelain=v1 -z` output and return
/// `true` if any entry references a path *not* in `allowlist`.
///
/// Uses `-z` so paths are emitted verbatim (no octal escaping, no surrounding
/// quotes) and entries are NUL-separated. Rename/copy entries include the
/// old path as a second NUL-terminated field; we treat that old-path field
/// as a second dirty entry and require it to be allowlisted too.
pub(crate) fn porcelain_has_non_allowlisted_entry(stdout: &[u8], allowlist: &[&str]) -> bool {
    let mut iter = stdout.split(|&b| b == 0).peekable();
    while let Some(entry) = iter.next() {
        if entry.is_empty() {
            // Trailing NUL (or blank line) — ignore.
            continue;
        }
        // Each entry is `XY<space>path` (2 status bytes, space, path).
        if entry.len() < 4 {
            return true;
        }
        let status = &entry[..2];
        let path_bytes = &entry[3..];
        let path = match std::str::from_utf8(path_bytes) {
            Ok(p) => p,
            // Non-UTF-8 path — can't match the allowlist, treat as dirty.
            Err(_) => return true,
        };
        if !allowlist.contains(&path) {
            return true;
        }
        // Rename (`R`) and copy (`C`) entries have the old path following in
        // the next NUL-terminated field. Consume and check it too.
        let is_rename_or_copy =
            status[0] == b'R' || status[0] == b'C' || status[1] == b'R' || status[1] == b'C';
        if is_rename_or_copy {
            match iter.next() {
                Some(old) if !old.is_empty() => match std::str::from_utf8(old) {
                    Ok(old_path) if allowlist.contains(&old_path) => continue,
                    _ => return true,
                },
                _ => return true,
            }
        }
    }
    false
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
    fn porcelain_clean_tree_is_not_dirty() {
        assert!(!porcelain_has_non_allowlisted_entry(
            b"",
            LIVE_WORKSPACE_FILES
        ));
    }

    #[test]
    fn porcelain_allowlisted_only_is_not_dirty() {
        // Two entries: modified config.toml, untracked ui_state.json.
        let stdout = b" M .ryve/config.toml\0?? .ryve/ui_state.json\0";
        assert!(!porcelain_has_non_allowlisted_entry(
            stdout,
            LIVE_WORKSPACE_FILES
        ));
    }

    #[test]
    fn porcelain_non_allowlisted_is_dirty() {
        // One allowlisted entry alongside one non-allowlisted entry.
        let stdout = b" M .ryve/config.toml\0 M src/main.rs\0";
        assert!(porcelain_has_non_allowlisted_entry(
            stdout,
            LIVE_WORKSPACE_FILES
        ));
    }

    #[test]
    fn porcelain_rename_old_path_must_also_be_allowlisted() {
        // Rename from an allowlisted path to a non-allowlisted path is dirty.
        let stdout = b"R  src/other.rs\0.ryve/config.toml\0";
        assert!(porcelain_has_non_allowlisted_entry(
            stdout,
            LIVE_WORKSPACE_FILES
        ));
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
