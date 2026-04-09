// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Thin wrapper around tmux for Ryve's private session management.
//!
//! Every tmux invocation goes through [`TmuxClient`] which enforces:
//!
//! - **Bundled binary**: the tmux path is set once at construction time.
//! - **Private socket**: all commands use `-S <state_dir>/tmux.sock` so Ryve
//!   never touches the user's default tmux server.
//!
//! The module exposes only the operations Ryve needs — it is not a generic
//! tmux client library.
//!
//! Some methods (`list_sessions`, `kill_session`, `attach_command`) are
//! currently only consumed by tests and downstream sparks (Attach UI,
//! session list CLI). They are part of the wrapper's required API per
//! spark ryve-4bae4ff6.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use data::sparks::types::PersistedAgentSession;
use sqlx::SqlitePool;

use crate::process_snapshot::ProcessSnapshot;

// ── Errors ────────────────────────────────────────────────

/// Typed errors for tmux operations.
#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    /// The configured tmux binary does not exist on disk.
    #[error("tmux binary not found at {0}")]
    BinaryMissing(PathBuf),

    /// Attempted to create a session that already exists.
    #[error("tmux session already exists: {0}")]
    SessionExists(String),

    /// Referenced a session that does not exist.
    #[error("tmux session not found: {0}")]
    SessionNotFound(String),

    /// Generic I/O error (spawn failure, pipe error, etc.).
    #[error("tmux I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// tmux exited with a non-zero status for a reason not covered above.
    #[error("tmux command failed (exit {exit_code}): {stderr}")]
    CommandFailed { exit_code: i32, stderr: String },
}

// ── Session info ──────────────────────────────────────────

/// Minimal information about a running tmux session.
/// Currently only used in tests; will be wired into production once
/// the full tmux session management UI is in place.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub name: String,
    pub created: String,
    pub attached: bool,
}

// ── Command runner trait (for testability) ─────────────────

/// Abstraction over process execution so unit tests can inject a mock.
pub trait CommandRunner: Send + Sync {
    /// Run a command and capture its output.
    fn run(&self, cmd: &str, args: &[&str]) -> std::io::Result<Output>;
}

/// Default implementation that shells out for real.
pub struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> std::io::Result<Output> {
        std::process::Command::new(cmd).args(args).output()
    }
}

// ── TmuxClient ────────────────────────────────────────────

/// A configured tmux client bound to a specific binary and private socket.
pub struct TmuxClient<R: CommandRunner = RealCommandRunner> {
    /// Path to the tmux binary.
    tmux_bin: PathBuf,
    /// Path to the Ryve-private socket file.
    socket_path: PathBuf,
    /// Injectable command runner.
    runner: R,
}

impl TmuxClient<RealCommandRunner> {
    /// Create a client using the real tmux binary.
    ///
    /// `tmux_bin` — absolute path to the bundled tmux binary.
    /// `state_dir` — the `.ryve/` directory. The socket is placed at a
    /// path short enough for Unix domain sockets (max ~104 chars on
    /// macOS). If `state_dir/tmux.sock` fits, we use it directly;
    /// otherwise we fall back to `/tmp/ryve-<hash>.sock`.
    pub fn new(tmux_bin: PathBuf, state_dir: &Path) -> Self {
        Self {
            tmux_bin,
            socket_path: short_socket_path(state_dir),
            runner: RealCommandRunner,
        }
    }
}

/// Compute a socket path short enough for Unix domain sockets.
/// macOS limits `sun_path` to 104 bytes. If the canonical path fits,
/// use `<state_dir>/tmux.sock`; otherwise hash the state_dir and put
/// the socket under `/tmp`.
fn short_socket_path(state_dir: &Path) -> PathBuf {
    let canonical = state_dir.join("tmux.sock");
    // 104 is the macOS limit; Linux is 108. Use the smaller.
    if canonical.to_string_lossy().len() <= 100 {
        return canonical;
    }
    // Hash the state_dir to get a unique, short name.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    state_dir.hash(&mut hasher);
    let hash = hasher.finish();
    PathBuf::from(format!("/tmp/ryve-{hash:016x}.sock"))
}

impl<R: CommandRunner> TmuxClient<R> {
    /// Create a client with an injectable command runner (for testing).
    #[cfg(test)]
    pub fn with_runner(tmux_bin: PathBuf, state_dir: &Path, runner: R) -> Self {
        Self {
            tmux_bin,
            socket_path: state_dir.join("tmux.sock"),
            runner,
        }
    }

    /// Base args that every tmux invocation must include.
    fn base_args(&self) -> Vec<String> {
        vec![
            "-S".to_string(),
            self.socket_path.to_string_lossy().into_owned(),
        ]
    }

    /// Run a tmux subcommand, returning the raw output.
    fn run_tmux(&self, subcmd_args: &[&str]) -> Result<Output, TmuxError> {
        if !self.tmux_bin.exists() {
            return Err(TmuxError::BinaryMissing(self.tmux_bin.clone()));
        }

        let base = self.base_args();
        let mut all_args: Vec<&str> = base.iter().map(String::as_str).collect();
        all_args.extend_from_slice(subcmd_args);

        let output = self
            .runner
            .run(self.tmux_bin.to_str().unwrap_or("tmux"), &all_args)?;

        Ok(output)
    }

    /// Run a tmux subcommand and require success (zero exit).
    fn run_tmux_ok(&self, subcmd_args: &[&str]) -> Result<Output, TmuxError> {
        let output = self.run_tmux(subcmd_args)?;
        if output.status.success() {
            Ok(output)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(TmuxError::CommandFailed {
                exit_code: output.status.code().unwrap_or(-1),
                stderr,
            })
        }
    }

    /// Create a new detached tmux session.
    ///
    /// - `name` — session name (must be unique).
    /// - `cwd` — working directory for the initial window.
    /// - `env` — extra environment variables to set inside the session.
    /// - `argv` — the command (and args) to run in the initial window.
    ///   If empty, the user's default shell is used.
    pub fn new_session_detached(
        &self,
        name: &str,
        cwd: &Path,
        env: &HashMap<String, String>,
        argv: &[&str],
    ) -> Result<(), TmuxError> {
        // Check whether the session already exists.
        if self.has_session(name)? {
            return Err(TmuxError::SessionExists(name.to_string()));
        }

        let mut args: Vec<&str> = vec![
            "new-session",
            "-d",
            "-s",
            name,
            "-c",
            cwd.to_str().unwrap_or("."),
        ];

        // Environment variables: tmux new-session accepts -e KEY=VALUE
        // (tmux 3.2+). We build owned strings and keep references.
        let env_pairs: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        for pair in &env_pairs {
            args.push("-e");
            args.push(pair.as_str());
        }

        // Append the command to run, if any.
        args.extend_from_slice(argv);

        self.run_tmux_ok(&args)?;
        Ok(())
    }

    /// Build a `std::process::Command` that, when executed, will attach to
    /// the named session. The caller owns the `Command` and can spawn it
    /// however they like (e.g. inside an `iced_term` PTY).
    pub fn attach_command(&self, name: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.tmux_bin);
        cmd.args(self.base_args());
        cmd.args(["attach-session", "-t", name]);
        cmd
    }

    /// Check whether a session with the given name exists.
    pub fn has_session(&self, name: &str) -> Result<bool, TmuxError> {
        let output = self.run_tmux(&["has-session", "-t", name])?;
        Ok(output.status.success())
    }

    /// Pipe the output of the current pane in `name` to a log file.
    ///
    /// Uses `pipe-pane` so tmux streams the pane's content to the given path.
    pub fn pipe_pane(&self, name: &str, log_path: &Path) -> Result<(), TmuxError> {
        if !self.has_session(name)? {
            return Err(TmuxError::SessionNotFound(name.to_string()));
        }
        let log_str = log_path.to_string_lossy();
        let pipe_cmd = format!("cat >> {log_str}");
        self.run_tmux_ok(&["pipe-pane", "-t", name, &pipe_cmd])?;
        Ok(())
    }
}

/// Methods used only in tests. Part of the TmuxClient API surface defined
/// by spark ryve-4bae4ff6 but not yet wired into production code paths.
#[cfg(test)]
impl<R: CommandRunner> TmuxClient<R> {
    /// List all sessions on the private socket.
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>, TmuxError> {
        let output = self.run_tmux(&[
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_created}\t#{session_attached}",
        ]);

        let output = match output {
            Ok(o) if o.status.success() => o,
            Ok(_) | Err(TmuxError::CommandFailed { .. }) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let sessions = stdout
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(3, '\t');
                let name = parts.next()?.to_string();
                let created = parts.next().unwrap_or("").to_string();
                let attached = parts.next().unwrap_or("0") != "0";
                Some(SessionInfo {
                    name,
                    created,
                    attached,
                })
            })
            .collect();

        Ok(sessions)
    }

    /// Kill (destroy) a session by name.
    pub fn kill_session(&self, name: &str) -> Result<(), TmuxError> {
        if !self.has_session(name)? {
            return Err(TmuxError::SessionNotFound(name.to_string()));
        }
        self.run_tmux_ok(&["kill-session", "-t", name])?;
        Ok(())
    }

    /// Return the path to the socket this client uses.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

/// Resolve the tmux binary path. Checks (in order):
/// 1. `RYVE_TMUX_BIN` env var
/// 2. `tmux` on PATH via `which`
pub fn resolve_tmux_bin() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("RYVE_TMUX_BIN") {
        let p = PathBuf::from(val);
        if p.exists() {
            return Some(p);
        }
    }
    which_tmux()
}

fn which_tmux() -> Option<PathBuf> {
    std::process::Command::new("which")
        .arg("tmux")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(PathBuf::from(s))
            }
        })
}

// ── Session naming ───────────────────────────────────────

/// The canonical tmux session name for a given agent session.
/// Format: `<label>-<short_id>` where short_id is the first 8 chars.
pub fn session_name(session_label: Option<&str>, session_id: &str) -> String {
    let label = session_label.unwrap_or("hand");
    let short = &session_id[..8.min(session_id.len())];
    format!("{label}-{short}")
}

/// Returns true if `name` looks like a Ryve-managed tmux session.
fn is_ryve_session(name: &str) -> bool {
    name.starts_with("hand-") || name.starts_with("head-") || name.starts_with("merger-")
}

// ── Async session listing ────────────────────────────────

/// A live tmux session discovered on the private socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSession {
    pub name: String,
}

/// List all tmux sessions on the workshop's private socket (async).
pub async fn list_sessions_async(workshop_dir: &Path) -> Vec<TmuxSession> {
    let sock = workshop_dir.join(".ryve").join("tmux.sock");
    if !sock.exists() {
        return Vec::new();
    }
    let output = tokio::process::Command::new("tmux")
        .args([
            "-S",
            &sock.to_string_lossy(),
            "list-sessions",
            "-F",
            "#{session_name}",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| TmuxSession {
                    name: l.trim().to_string(),
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

// ── Reconciliation ───────────────────────────────────────

/// Outcome of one reconciliation run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileResult {
    pub confirmed_live: Vec<String>,
    pub marked_stopped: Vec<String>,
    pub orphaned_tmux: Vec<String>,
}

/// Reconcile existing tmux sessions with `agent_sessions` DB rows.
///
/// Called on app boot to restore liveness state after a Ryve restart.
pub async fn reconcile_sessions(
    workshop_dir: &Path,
    pool: &SqlitePool,
    workshop_id: &str,
) -> ReconcileResult {
    let mut result = ReconcileResult::default();

    let live_tmux = list_sessions_async(workshop_dir).await;
    let live_names: HashSet<&str> = live_tmux.iter().map(|s| s.name.as_str()).collect();

    let db_sessions: Vec<PersistedAgentSession> =
        data::sparks::agent_session_repo::list_for_workshop(pool, workshop_id)
            .await
            .unwrap_or_default();

    let active_sessions: Vec<&PersistedAgentSession> = db_sessions
        .iter()
        .filter(|s| s.status == "active")
        .collect();

    let mut expected_names: HashSet<String> = HashSet::new();

    for s in &active_sessions {
        let name = session_name(s.session_label.as_deref(), &s.id);
        expected_names.insert(name.clone());

        if live_names.contains(name.as_str()) {
            log::info!(
                "tmux reconcile: session '{name}' (db={}) confirmed live",
                s.id
            );
            result.confirmed_live.push(s.id.clone());
        } else {
            log::info!(
                "tmux reconcile: session '{name}' (db={}) has no tmux session — marking stopped",
                s.id
            );
            let _ = data::sparks::agent_session_repo::end_session(pool, &s.id).await;
            result.marked_stopped.push(s.id.clone());
        }
    }

    for ts in &live_tmux {
        if is_ryve_session(&ts.name) && !expected_names.contains(&ts.name) {
            log::warn!(
                "tmux reconcile: tmux session '{}' has no matching agent_sessions row — leaving alone",
                ts.name
            );
            result.orphaned_tmux.push(ts.name.clone());
        }
    }

    result
}

// ── Convenience free functions ────────────────────────────
//
// These wrap `TmuxClient` so callers in the UI layer do not need to
// construct a client each time.

/// Derive the tmux session name from a label and session id.
pub fn session_name_for(label: &str, session_id: &str) -> String {
    format!("{label}-{session_id}")
}

/// Check whether a tmux session exists on the Ryve-private socket.
pub fn has_session(workshop_dir: &Path, session_name: &str) -> bool {
    let Some(bin) = resolve_tmux_bin() else {
        return false;
    };
    let client = TmuxClient::new(bin, &workshop_dir.join(".ryve"));
    client.has_session(session_name).unwrap_or(false)
}

/// Build a command that attaches to the named tmux session.
/// Returns `(program, args)` suitable for spawning.
pub fn attach_command(workshop_dir: &Path, session_name: &str) -> (String, Vec<String>) {
    let bin = resolve_tmux_bin().unwrap_or_else(|| PathBuf::from("tmux"));
    let client = TmuxClient::new(bin, &workshop_dir.join(".ryve"));
    let cmd = client.attach_command(session_name);
    let program = cmd.get_program().to_string_lossy().into_owned();
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    (program, args)
}

// ── PID-based liveness detection ─────────────────────────

/// Diff tracked sessions against a process snapshot and return session IDs
/// whose process has disappeared. Each entry in `tracked` is
/// `(session_id, child_pid)`.
pub fn dead_sessions(tracked: &[(String, Option<i64>)], snapshot: &ProcessSnapshot) -> Vec<String> {
    tracked
        .iter()
        .filter(|(_, pid)| !pid.map(|p| snapshot.is_alive(p)).unwrap_or(false))
        .map(|(id, _)| id.clone())
        .collect()
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn ok_output(stdout: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn err_output(code: i32, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// A simple mock command runner that delegates to a closure.
    struct MockRunner {
        handler: Box<dyn Fn(&str, &[&str]) -> std::io::Result<Output> + Send + Sync>,
    }

    impl CommandRunner for MockRunner {
        fn run(&self, cmd: &str, args: &[&str]) -> std::io::Result<Output> {
            (self.handler)(cmd, args)
        }
    }

    fn make_client(
        handler: impl Fn(&str, &[&str]) -> std::io::Result<Output> + Send + Sync + 'static,
    ) -> TmuxClient<MockRunner> {
        let tmp = std::env::temp_dir();
        let fake_bin = tmp.join("fake-tmux-for-test");
        std::fs::write(&fake_bin, b"").unwrap();
        TmuxClient::with_runner(
            fake_bin,
            &tmp,
            MockRunner {
                handler: Box::new(handler),
            },
        )
    }

    #[test]
    fn binary_missing_error() {
        let client = TmuxClient::with_runner(
            PathBuf::from("/nonexistent/tmux"),
            Path::new("/tmp"),
            MockRunner {
                handler: Box::new(|_, _| Ok(ok_output(""))),
            },
        );
        let result = client.has_session("test");
        assert!(matches!(result, Err(TmuxError::BinaryMissing(_))));
    }

    #[test]
    fn has_session_returns_true_on_success() {
        let client = make_client(|_, _| Ok(ok_output("")));
        assert!(client.has_session("my-session").unwrap());
    }

    #[test]
    fn has_session_returns_false_on_nonzero_exit() {
        let client = make_client(|_, _| Ok(err_output(1, "session not found")));
        assert!(!client.has_session("missing").unwrap());
    }

    #[test]
    fn list_sessions_parses_output() {
        let client = make_client(|_, args: &[&str]| {
            if args.iter().any(|a| *a == "list-sessions") {
                Ok(ok_output("sess1\t1700000000\t0\nsess2\t1700000001\t1\n"))
            } else {
                Ok(ok_output(""))
            }
        });
        let sessions = client.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "sess1");
        assert!(!sessions[0].attached);
        assert_eq!(sessions[1].name, "sess2");
        assert!(sessions[1].attached);
    }

    #[test]
    fn list_sessions_empty_on_no_server() {
        let client = make_client(|_, _| Ok(err_output(1, "no server running")));
        let sessions = client.list_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn new_session_detached_errors_on_existing() {
        let client = make_client(|_, _| Ok(ok_output("")));
        let result = client.new_session_detached("dup", Path::new("/tmp"), &HashMap::new(), &[]);
        assert!(matches!(result, Err(TmuxError::SessionExists(_))));
    }

    #[test]
    fn new_session_detached_success() {
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();
        let client = make_client(move |_, args: &[&str]| {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(err_output(1, "")) // has_session → not found
            } else {
                assert!(args.iter().any(|a| *a == "new-session"));
                assert!(args.iter().any(|a| *a == "-d"));
                Ok(ok_output(""))
            }
        });
        client
            .new_session_detached("test", Path::new("/work"), &HashMap::new(), &["bash"])
            .unwrap();
    }

    #[test]
    fn kill_session_not_found() {
        let client = make_client(|_, _| Ok(err_output(1, "")));
        let result = client.kill_session("ghost");
        assert!(matches!(result, Err(TmuxError::SessionNotFound(_))));
    }

    #[test]
    fn kill_session_success() {
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();
        let client = make_client(move |_, args: &[&str]| {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(ok_output("")) // has_session → exists
            } else {
                assert!(args.iter().any(|a| *a == "kill-session"));
                Ok(ok_output(""))
            }
        });
        client.kill_session("doomed").unwrap();
    }

    #[test]
    fn pipe_pane_not_found() {
        let client = make_client(|_, _| Ok(err_output(1, "")));
        let result = client.pipe_pane("ghost", Path::new("/tmp/log"));
        assert!(matches!(result, Err(TmuxError::SessionNotFound(_))));
    }

    #[test]
    fn pipe_pane_success() {
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();
        let client = make_client(move |_, args: &[&str]| {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(ok_output("")) // has_session
            } else {
                assert!(args.iter().any(|a| *a == "pipe-pane"));
                Ok(ok_output(""))
            }
        });
        client
            .pipe_pane("my-sess", Path::new("/tmp/session.log"))
            .unwrap();
    }

    #[test]
    fn attach_command_uses_private_socket() {
        let client = TmuxClient::with_runner(
            PathBuf::from("/usr/bin/tmux"),
            Path::new("/home/user/.ryve"),
            MockRunner {
                handler: Box::new(|_, _| Ok(ok_output(""))),
            },
        );
        let cmd = client.attach_command("my-sess");
        let prog = cmd.get_program().to_string_lossy().to_string();
        assert_eq!(prog, "/usr/bin/tmux");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"-S".to_string()));
        assert!(args.contains(&"/home/user/.ryve/tmux.sock".to_string()));
        assert!(args.contains(&"attach-session".to_string()));
        assert!(args.contains(&"my-sess".to_string()));
    }

    #[test]
    fn new_session_passes_env_vars() {
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();
        let client = make_client(move |_, args: &[&str]| {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(err_output(1, "")) // has_session → not found
            } else {
                assert!(
                    args.iter().any(|a| *a == "-e"),
                    "expected -e flags for env vars"
                );
                Ok(ok_output(""))
            }
        });
        let mut env = HashMap::new();
        env.insert("RYVE_WORKSHOP_ROOT".to_string(), "/work".to_string());
        client
            .new_session_detached("test", Path::new("/work"), &env, &[])
            .unwrap();
    }
}
