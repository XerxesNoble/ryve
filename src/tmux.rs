// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Tmux wrapper for launching and managing Ryve agent sessions.
//!
//! Ryve launches Heads (and, in future, Hands) inside tmux sessions on a
//! per-workshop private socket. These sessions survive Ryve restarts,
//! making agents recallable and enabling Atlas/Director handoffs across
//! process boundaries.
//!
//! All tmux invocations go through this module using the Ryve-private
//! socket so they never touch the user's default tmux server.
//!
//! Spark ryve-7e1854c7 / [sp-0285181c].

use std::path::{Path, PathBuf};
use std::process::Command;

/// Errors from tmux operations.
#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    #[error("tmux binary not found — is tmux installed?")]
    BinaryMissing,
    #[error("tmux session already exists: {0}")]
    SessionExists(String),
    #[error("tmux session not found: {0}")]
    SessionNotFound(String),
    #[error("tmux command failed (exit {status}): {stderr}")]
    CommandFailed { status: i32, stderr: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Path to the Ryve-private tmux socket.
///
/// Unix domain sockets have a ~104-byte path limit. Workshop directories
/// can easily exceed that (especially temp dirs in tests), so we place the
/// socket under `/tmp/ryve-tmux-<hash>.sock` where `<hash>` is a short
/// deterministic hash of the workshop directory. This keeps the path well
/// under the limit while still giving each workshop its own socket.
pub fn socket_path(workshop_dir: &Path) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    workshop_dir.hash(&mut hasher);
    let hash = hasher.finish();
    PathBuf::from(format!("/tmp/ryve-tmux-{hash:016x}.sock"))
}

/// Create a new detached tmux session on the Ryve-private socket.
///
/// The session runs `argv[0] argv[1..]` inside `cwd` with the given
/// environment variables layered on top of the inherited env.
pub fn new_session_detached(
    workshop_dir: &Path,
    session_name: &str,
    cwd: &Path,
    env: &[(String, String)],
    argv: &[String],
) -> Result<(), TmuxError> {
    let socket = socket_path(workshop_dir);

    // Build the shell command that sets env vars and then exec's the agent.
    let env_prefix: String = env
        .iter()
        .map(|(k, v)| format!("{}={}", k, shell_escape(v)))
        .collect::<Vec<_>>()
        .join(" ");

    let argv_escaped: String = argv
        .iter()
        .map(|a| shell_escape(a))
        .collect::<Vec<_>>()
        .join(" ");

    let shell_cmd = if env_prefix.is_empty() {
        format!("exec {argv_escaped}")
    } else {
        format!("exec env {env_prefix} {argv_escaped}")
    };

    let mut cmd = Command::new("tmux");
    cmd.args([
        "-S",
        &socket.to_string_lossy(),
        "new-session",
        "-d", // detached
        "-s",
        session_name,
        "-c",
        &cwd.to_string_lossy(),
        "--",
        "sh",
        "-c",
        &shell_cmd,
    ]);

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            TmuxError::BinaryMissing
        } else {
            TmuxError::Io(e)
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if stderr.contains("duplicate session") {
            return Err(TmuxError::SessionExists(session_name.to_string()));
        }
        return Err(TmuxError::CommandFailed {
            status: output.status.code().unwrap_or(-1),
            stderr,
        });
    }

    Ok(())
}

/// Configure pipe-pane on a tmux session so all output is teed to a log file.
///
/// Uses `tmux pipe-pane -t <session> 'cat >> <log_path>'` so the log is
/// written continuously even when no one is attached.
pub fn pipe_pane(
    workshop_dir: &Path,
    session_name: &str,
    log_path: &Path,
) -> Result<(), TmuxError> {
    let socket = socket_path(workshop_dir);

    let output = Command::new("tmux")
        .args([
            "-S",
            &socket.to_string_lossy(),
            "pipe-pane",
            "-t",
            session_name,
            &format!("cat >> {}", shell_escape(&log_path.to_string_lossy())),
        ])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TmuxError::BinaryMissing
            } else {
                TmuxError::Io(e)
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if stderr.contains("session not found") || stderr.contains("can't find session") {
            return Err(TmuxError::SessionNotFound(session_name.to_string()));
        }
        return Err(TmuxError::CommandFailed {
            status: output.status.code().unwrap_or(-1),
            stderr,
        });
    }

    Ok(())
}

/// Kill a tmux session on the Ryve-private socket.
///
/// Currently used by test cleanup; will be consumed by the Hand tmux
/// spark and the reconciliation module.
#[cfg(test)]
pub fn kill_session(workshop_dir: &Path, session_name: &str) -> Result<(), TmuxError> {
    let socket = socket_path(workshop_dir);

    let output = Command::new("tmux")
        .args([
            "-S",
            &socket.to_string_lossy(),
            "kill-session",
            "-t",
            session_name,
        ])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TmuxError::BinaryMissing
            } else {
                TmuxError::Io(e)
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if stderr.contains("session not found") || stderr.contains("can't find session") {
            return Err(TmuxError::SessionNotFound(session_name.to_string()));
        }
        return Err(TmuxError::CommandFailed {
            status: output.status.code().unwrap_or(-1),
            stderr,
        });
    }

    Ok(())
}

/// Minimal shell escaping: wrap in single quotes, escaping any embedded
/// single quotes with the `'\''` idiom.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_escape_handles_simple_strings() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn shell_escape_handles_single_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_escape_handles_spaces_and_special_chars() {
        assert_eq!(shell_escape("a b$c"), "'a b$c'");
    }

    #[test]
    fn socket_path_is_deterministic_and_short() {
        let p = socket_path(Path::new("/tmp/workshop"));
        assert!(
            p.to_string_lossy().len() < 104,
            "socket path must be under the Unix domain socket limit"
        );
        let p2 = socket_path(Path::new("/tmp/workshop"));
        assert_eq!(p, p2);
        let p3 = socket_path(Path::new("/tmp/other-workshop"));
        assert_ne!(p, p3);
    }
}
