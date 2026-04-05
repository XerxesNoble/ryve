// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Git and worktree operations via the `git` CLI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct Repository {
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Worktree {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
}

/// Git status of a single file relative to the repo root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Ignored,
    Conflicted,
}

/// Per-file diff statistics (line additions and deletions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStat {
    pub additions: u32,
    pub deletions: u32,
}

/// Per-line diff annotation for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineChange {
    /// Line was added (not in HEAD).
    Added,
    /// Line was modified (differs from HEAD).
    Modified,
    /// One or more lines were deleted after this line.
    Deleted,
}

impl Repository {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// List all worktrees for this repository.
    pub async fn list_worktrees(&self) -> Result<Vec<Worktree>, GitError> {
        let output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&self.path)
            .output()
            .await
            .map_err(GitError::Io)?;

        if !output.status.success() {
            return Err(GitError::Command(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_worktree_list(&stdout))
    }

    /// Get the current branch name (or HEAD if detached).
    pub async fn current_branch(&self) -> Result<String, GitError> {
        let output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&self.path)
            .output()
            .await
            .map_err(GitError::Io)?;

        if !output.status.success() {
            return Err(GitError::Command(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Get file-level git status for the working tree.
    /// Returns a map from repo-relative file path to its status.
    pub async fn file_statuses(&self) -> Result<HashMap<PathBuf, FileStatus>, GitError> {
        let output = Command::new("git")
            .args(["status", "--porcelain=v1", "-uall"])
            .current_dir(&self.path)
            .output()
            .await
            .map_err(GitError::Io)?;

        if !output.status.success() {
            return Err(GitError::Command(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_status_porcelain(&stdout))
    }

    /// Get per-line diff annotations for a file against HEAD.
    /// Returns a map from 1-based line number to the type of change.
    pub async fn line_diff(&self, file_path: &Path) -> Result<HashMap<u32, LineChange>, GitError> {
        let output = Command::new("git")
            .args(["diff", "HEAD", "--unified=0", "--"])
            .arg(file_path)
            .current_dir(&self.path)
            .output()
            .await
            .map_err(GitError::Io)?;

        if !output.status.success() {
            // Not tracked or no HEAD yet — treat entire file as added
            return Ok(HashMap::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_unified_diff(&stdout))
    }

    /// Get per-file diff stats (additions, deletions) for the working tree against HEAD.
    /// Returns a map from repo-relative file path to (additions, deletions).
    pub async fn diff_stats(&self) -> Result<HashMap<PathBuf, DiffStat>, GitError> {
        // Staged changes
        let staged = Command::new("git")
            .args(["diff", "--cached", "--numstat"])
            .current_dir(&self.path)
            .output()
            .await
            .map_err(GitError::Io)?;

        // Unstaged changes
        let unstaged = Command::new("git")
            .args(["diff", "--numstat"])
            .current_dir(&self.path)
            .output()
            .await
            .map_err(GitError::Io)?;

        let mut stats = HashMap::new();

        for output in [&staged, &unstaged] {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if let Some(stat) = parse_numstat_line(line) {
                        let entry = stats.entry(stat.0).or_insert(DiffStat { additions: 0, deletions: 0 });
                        entry.additions += stat.1.additions;
                        entry.deletions += stat.1.deletions;
                    }
                }
            }
        }

        Ok(stats)
    }

    /// Check if a path is inside a git repository.
    pub async fn is_repo(path: &Path) -> bool {
        Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(path)
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// Parse `git status --porcelain=v1` output into a file status map.
fn parse_status_porcelain(output: &str) -> HashMap<PathBuf, FileStatus> {
    let mut statuses = HashMap::new();

    for line in output.lines() {
        if line.len() < 4 {
            continue;
        }

        let index = line.as_bytes()[0];
        let worktree = line.as_bytes()[1];
        let file_path = &line[3..];

        // For renames, take the destination path (after " -> ")
        let path = if let Some(pos) = file_path.find(" -> ") {
            PathBuf::from(&file_path[pos + 4..])
        } else {
            PathBuf::from(file_path)
        };

        let status = match (index, worktree) {
            (b'?', b'?') => FileStatus::Untracked,
            (b'!', b'!') => FileStatus::Ignored,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => FileStatus::Conflicted,
            (b'A', _) => FileStatus::Added,
            (_, b'D') | (b'D', _) => FileStatus::Deleted,
            (b'R', _) => FileStatus::Renamed,
            (b'C', _) => FileStatus::Copied,
            _ => FileStatus::Modified,
        };

        statuses.insert(path, status);
    }

    statuses
}

fn parse_worktree_list(output: &str) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;

    for line in output.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            branch = Some(b.to_string());
        } else if line.is_empty() {
            if let (Some(p), Some(b)) = (path.take(), branch.take()) {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                worktrees.push(Worktree {
                    name,
                    path: p,
                    branch: b,
                });
            }
            path = None;
            branch = None;
        }
    }

    worktrees
}

/// Parse `git diff --unified=0` output into per-line change annotations.
/// Handles @@ hunk headers to determine which lines were added/modified/deleted.
fn parse_unified_diff(output: &str) -> HashMap<u32, LineChange> {
    let mut changes = HashMap::new();

    for line in output.lines() {
        if !line.starts_with("@@") {
            continue;
        }

        // Parse "@@ -old_start[,old_count] +new_start[,new_count] @@"
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }

        let old_part = parts[1]; // e.g., "-10,3" or "-10"
        let new_part = parts[2]; // e.g., "+12,5" or "+12"

        let (old_count,) = parse_hunk_range(old_part);
        let (new_start, new_count) = parse_hunk_start_and_count(new_part);

        if old_count == 0 && new_count > 0 {
            // Pure addition
            for i in 0..new_count {
                changes.insert(new_start + i, LineChange::Added);
            }
        } else if old_count > 0 && new_count == 0 {
            // Pure deletion — mark the line after which content was removed
            let marker = if new_start == 0 { 1 } else { new_start };
            changes.entry(marker).or_insert(LineChange::Deleted);
        } else {
            // Modification — lines were changed
            for i in 0..new_count {
                changes.insert(new_start + i, LineChange::Modified);
            }
            // If old had more lines than new, mark deletion at end of new range
            if old_count > new_count {
                let marker = new_start + new_count;
                changes.entry(marker).or_insert(LineChange::Deleted);
            }
        }
    }

    changes
}

fn parse_hunk_range(part: &str) -> (u32,) {
    // "-10,3" → count=3, "-10" → count=1
    let s = part.trim_start_matches(['-', '+']);
    if let Some((_start, count)) = s.split_once(',') {
        (count.parse().unwrap_or(1),)
    } else {
        (1,)
    }
}

fn parse_hunk_start_and_count(part: &str) -> (u32, u32) {
    // "+12,5" → (12, 5), "+12" → (12, 1)
    let s = part.trim_start_matches(['-', '+']);
    if let Some((start, count)) = s.split_once(',') {
        (
            start.parse().unwrap_or(1),
            count.parse().unwrap_or(1),
        )
    } else {
        (s.parse().unwrap_or(1), 1)
    }
}

/// Parse a single line of `git diff --numstat` output.
/// Format: `<added>\t<deleted>\t<path>` (binary files show `-\t-\t<path>`)
fn parse_numstat_line(line: &str) -> Option<(PathBuf, DiffStat)> {
    let mut parts = line.splitn(3, '\t');
    let added_str = parts.next()?;
    let deleted_str = parts.next()?;
    let path_str = parts.next()?;

    // Binary files show "-" for both counts — skip them
    let additions = added_str.parse::<u32>().ok()?;
    let deletions = deleted_str.parse::<u32>().ok()?;

    // Handle renames: "old_path => new_path" or "{old => new}/path"
    let path = if let Some(pos) = path_str.find(" => ") {
        // Simple rename: take the new path
        PathBuf::from(&path_str[pos + 4..])
    } else {
        PathBuf::from(path_str)
    };

    Some((path, DiffStat { additions, deletions }))
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git command failed: {0}")]
    Command(String),
}
