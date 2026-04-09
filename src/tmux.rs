// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Thin Rust wrapper around the tmux binary.
//!
//! Every tmux invocation in Ryve goes through this module. All commands use a
//! Ryve-private socket (`-S <ryve-state-dir>/tmux.sock`) so they never touch
//! the user's default tmux server.
//!
//! The module is designed for testability: a [`CommandRunner`] trait abstracts
//! process execution so unit tests can inject a mock without touching the real
//! binary.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Output;

/// Information about a running tmux session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub name: String,
}

/// Errors produced by the tmux wrapper.
#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    #[error("tmux binary not found at {0}")]
    BinaryMissing(PathBuf),

    #[error("tmux session already exists: {0}")]
    SessionExists(String),

    #[error("tmux session not found: {0}")]
    SessionNotFound(String),

    #[error("tmux I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Abstraction over process execution so the tmux wrapper can be unit-tested
/// without spawning real processes.
pub trait CommandRunner: Send + Sync {
    /// Run a command to completion and return its output.
    fn run(&self, cmd: &mut std::process::Command) -> std::io::Result<Output>;
}

/// Default runner that delegates to the OS.
pub struct OsRunner;

impl CommandRunner for OsRunner {
    fn run(&self, cmd: &mut std::process::Command) -> std::io::Result<Output> {
        cmd.output()
    }
}

/// A handle to the tmux binary with Ryve-private socket discipline.
pub struct Tmux<R: CommandRunner = OsRunner> {
    /// Path to the tmux binary.
    binary: PathBuf,
    /// Path to the Ryve-private socket file.
    socket: PathBuf,
    /// Command runner (real or mock).
    runner: R,
}

impl Tmux<OsRunner> {
    /// Create a new `Tmux` handle using the OS command runner.
    ///
    /// `binary` is the path to the tmux executable (typically bundled alongside
    /// the Ryve binary). `state_dir` is the `.ryve/` directory; the socket is
    /// placed at `<state_dir>/tmux.sock`.
    pub fn new(binary: impl Into<PathBuf>, state_dir: impl AsRef<Path>) -> Self {
        Self::with_runner(binary, state_dir, OsRunner)
    }
}

impl<R: CommandRunner> Tmux<R> {
    /// Create a `Tmux` handle with a custom command runner (for testing).
    pub fn with_runner(binary: impl Into<PathBuf>, state_dir: impl AsRef<Path>, runner: R) -> Self {
        Self {
            binary: binary.into(),
            socket: state_dir.as_ref().join("tmux.sock"),
            runner,
        }
    }

    /// Build a base `Command` with the socket flag baked in.
    fn base_cmd(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.binary);
        cmd.arg("-S").arg(&self.socket);
        cmd
    }

    /// Create a new detached tmux session.
    ///
    /// - `name`: session name (must be unique on this socket).
    /// - `cwd`: working directory for the initial window.
    /// - `env`: extra environment variables injected into the session.
    /// - `argv`: the command (and arguments) to run in the initial window.
    ///   If empty, the user's default shell is used.
    pub fn new_session_detached(
        &self,
        name: &str,
        cwd: &Path,
        env: &HashMap<String, String>,
        argv: &[impl AsRef<OsStr>],
    ) -> Result<(), TmuxError> {
        self.check_binary()?;

        let mut cmd = self.base_cmd();
        cmd.arg("new-session")
            .arg("-d")
            .arg("-s")
            .arg(name)
            .arg("-c")
            .arg(cwd);

        for (key, val) in env {
            cmd.arg("-e").arg(format!("{key}={val}"));
        }

        if !argv.is_empty() {
            for arg in argv {
                cmd.arg(arg);
            }
        }

        let output = self.runner.run(&mut cmd)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("duplicate session") || stderr.contains("already exists") {
                return Err(TmuxError::SessionExists(name.to_owned()));
            }
            return Err(TmuxError::Io(std::io::Error::other(format!(
                "tmux new-session failed: {stderr}"
            ))));
        }

        Ok(())
    }

    /// Return a `Command` configured to attach to the named session.
    ///
    /// The caller is responsible for spawning / exec-ing the command (e.g. in
    /// a terminal widget). This intentionally returns a *not-yet-run* command
    /// so the caller can add stdio redirection, environment, etc.
    pub fn attach_command(&self, name: &str) -> std::process::Command {
        let mut cmd = self.base_cmd();
        cmd.arg("attach-session").arg("-t").arg(name);
        cmd
    }

    /// List all sessions on the Ryve-private socket.
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>, TmuxError> {
        self.check_binary()?;

        let mut cmd = self.base_cmd();
        cmd.arg("list-sessions").arg("-F").arg("#{session_name}");

        let output = self.runner.run(&mut cmd)?;

        // tmux exits non-zero when the server has no sessions — treat as empty.
        if !output.status.success() {
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let sessions = stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| SessionInfo { name: l.to_owned() })
            .collect();

        Ok(sessions)
    }

    /// Check whether a session with the given name exists.
    pub fn has_session(&self, name: &str) -> Result<bool, TmuxError> {
        self.check_binary()?;

        let mut cmd = self.base_cmd();
        cmd.arg("has-session").arg("-t").arg(name);

        let output = self.runner.run(&mut cmd)?;
        Ok(output.status.success())
    }

    /// Kill (destroy) the named session.
    pub fn kill_session(&self, name: &str) -> Result<(), TmuxError> {
        self.check_binary()?;

        let mut cmd = self.base_cmd();
        cmd.arg("kill-session").arg("-t").arg(name);

        let output = self.runner.run(&mut cmd)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("session not found") || stderr.contains("can't find session") {
                return Err(TmuxError::SessionNotFound(name.to_owned()));
            }
            return Err(TmuxError::Io(std::io::Error::other(format!(
                "tmux kill-session failed: {stderr}"
            ))));
        }

        Ok(())
    }

    /// Pipe the active pane's output to a log file.
    ///
    /// Uses `pipe-pane` to tee all output from the first pane of the named
    /// session into `log_path`. Useful for capturing Hand/Head agent output.
    pub fn pipe_pane(&self, name: &str, log_path: &Path) -> Result<(), TmuxError> {
        self.check_binary()?;

        let mut cmd = self.base_cmd();
        cmd.arg("pipe-pane")
            .arg("-t")
            .arg(name)
            .arg(format!("cat >> {}", log_path.display()));

        let output = self.runner.run(&mut cmd)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("session not found") || stderr.contains("can't find session") {
                return Err(TmuxError::SessionNotFound(name.to_owned()));
            }
            return Err(TmuxError::Io(std::io::Error::other(format!(
                "tmux pipe-pane failed: {stderr}"
            ))));
        }

        Ok(())
    }

    /// Validate that the tmux binary exists on disk.
    fn check_binary(&self) -> Result<(), TmuxError> {
        if !self.binary.exists() {
            return Err(TmuxError::BinaryMissing(self.binary.clone()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use super::*;

    /// A mock command runner that records calls and returns pre-configured
    /// responses.
    struct MockRunner {
        responses: std::sync::Mutex<Vec<std::io::Result<Output>>>,
        calls: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl MockRunner {
        fn new(responses: Vec<std::io::Result<Output>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn ok_output(stdout: &str) -> Output {
            Output {
                status: ExitStatus::from_raw(0),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            }
        }

        fn err_output(stderr: &str) -> Output {
            Output {
                status: ExitStatus::from_raw(256), // exit code 1
                stdout: Vec::new(),
                stderr: stderr.as_bytes().to_vec(),
            }
        }

        fn recorded_calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CommandRunner for MockRunner {
        fn run(&self, cmd: &mut std::process::Command) -> std::io::Result<Output> {
            // Record the full command line.
            let program = cmd.get_program().to_string_lossy().to_string();
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().to_string())
                .collect();
            let mut full = vec![program];
            full.extend(args);
            self.calls.lock().unwrap().push(full);

            self.responses
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| Ok(MockRunner::ok_output("")))
        }
    }

    /// Helper: build a `Tmux` with a shared mock runner.
    fn make_tmux(responses: Vec<std::io::Result<Output>>) -> Tmux<std::sync::Arc<MockRunner>> {
        let mut reversed = responses;
        reversed.reverse();
        let runner = std::sync::Arc::new(MockRunner::new(reversed));
        Tmux::with_runner("/bin/sh", "/tmp/ryve-test", std::sync::Arc::clone(&runner))
    }

    /// Helper: build a `Tmux` and return the shared runner for inspection.
    fn make_tmux_with_runner(
        responses: Vec<std::io::Result<Output>>,
    ) -> (Tmux<std::sync::Arc<MockRunner>>, std::sync::Arc<MockRunner>) {
        let mut reversed = responses;
        reversed.reverse();
        let runner = std::sync::Arc::new(MockRunner::new(reversed));
        let tmux = Tmux::with_runner("/bin/sh", "/tmp/ryve-test", std::sync::Arc::clone(&runner));
        (tmux, runner)
    }

    impl CommandRunner for std::sync::Arc<MockRunner> {
        fn run(&self, cmd: &mut std::process::Command) -> std::io::Result<Output> {
            (**self).run(cmd)
        }
    }

    #[test]
    fn socket_path_is_constructed_from_state_dir() {
        let tmux = make_tmux(vec![]);
        assert_eq!(tmux.socket, PathBuf::from("/tmp/ryve-test/tmux.sock"));
    }

    #[test]
    fn new_session_sends_correct_args() {
        let (tmux, runner) = make_tmux_with_runner(vec![Ok(MockRunner::ok_output(""))]);

        let mut env = HashMap::new();
        env.insert("FOO".into(), "bar".into());

        tmux.new_session_detached(
            "test-sess",
            Path::new("/home/user/project"),
            &env,
            &["bash", "--norc"],
        )
        .unwrap();

        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 1);
        let args = &calls[0];

        // Binary
        assert_eq!(args[0], "/bin/sh");
        // Socket flag
        assert_eq!(args[1], "-S");
        assert_eq!(args[2], "/tmp/ryve-test/tmux.sock");
        // Subcommand
        assert_eq!(args[3], "new-session");
        assert_eq!(args[4], "-d");
        assert_eq!(args[5], "-s");
        assert_eq!(args[6], "test-sess");
        assert_eq!(args[7], "-c");
        assert_eq!(args[8], "/home/user/project");
        // Env
        assert_eq!(args[9], "-e");
        assert_eq!(args[10], "FOO=bar");
        // argv
        assert_eq!(args[11], "bash");
        assert_eq!(args[12], "--norc");
    }

    #[test]
    fn new_session_duplicate_returns_session_exists() {
        let tmux = make_tmux(vec![Ok(MockRunner::err_output(
            "duplicate session: test-sess",
        ))]);

        let result =
            tmux.new_session_detached("test-sess", Path::new("/tmp"), &HashMap::new(), &["sh"]);

        assert!(matches!(result, Err(TmuxError::SessionExists(ref n)) if n == "test-sess"));
    }

    #[test]
    fn list_sessions_parses_output() {
        let tmux = make_tmux(vec![Ok(MockRunner::ok_output("alpha\nbeta\n"))]);

        let sessions = tmux.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "alpha");
        assert_eq!(sessions[1].name, "beta");
    }

    #[test]
    fn list_sessions_returns_empty_on_no_server() {
        let tmux = make_tmux(vec![Ok(MockRunner::err_output("no server running"))]);

        let sessions = tmux.list_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn has_session_returns_true_on_success() {
        let tmux = make_tmux(vec![Ok(MockRunner::ok_output(""))]);
        assert!(tmux.has_session("sess").unwrap());
    }

    #[test]
    fn has_session_returns_false_on_failure() {
        let tmux = make_tmux(vec![Ok(MockRunner::err_output("session not found"))]);
        assert!(!tmux.has_session("nope").unwrap());
    }

    #[test]
    fn kill_session_success() {
        let tmux = make_tmux(vec![Ok(MockRunner::ok_output(""))]);
        tmux.kill_session("doomed").unwrap();
    }

    #[test]
    fn kill_session_not_found() {
        let tmux = make_tmux(vec![Ok(MockRunner::err_output("session not found: gone"))]);

        let result = tmux.kill_session("gone");
        assert!(matches!(result, Err(TmuxError::SessionNotFound(ref n)) if n == "gone"));
    }

    #[test]
    fn pipe_pane_sends_correct_args() {
        let (tmux, runner) = make_tmux_with_runner(vec![Ok(MockRunner::ok_output(""))]);

        tmux.pipe_pane("sess", Path::new("/tmp/log.txt")).unwrap();

        let calls = runner.recorded_calls();
        assert_eq!(calls.len(), 1);
        let args = &calls[0];
        assert_eq!(args[3], "pipe-pane");
        assert_eq!(args[4], "-t");
        assert_eq!(args[5], "sess");
        assert_eq!(args[6], "cat >> /tmp/log.txt");
    }

    #[test]
    fn attach_command_returns_configured_command() {
        let tmux = make_tmux(vec![]);

        let cmd = tmux.attach_command("my-sess");
        assert_eq!(cmd.get_program(), "/bin/sh");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            args,
            vec![
                "-S",
                "/tmp/ryve-test/tmux.sock",
                "attach-session",
                "-t",
                "my-sess"
            ]
        );
    }

    #[test]
    fn binary_missing_error() {
        let tmux = Tmux::with_runner(
            "/nonexistent/tmux",
            "/tmp/ryve-test",
            std::sync::Arc::new(MockRunner::new(vec![])),
        );

        let result = tmux.new_session_detached("s", Path::new("/tmp"), &HashMap::new(), &["sh"]);

        assert!(matches!(result, Err(TmuxError::BinaryMissing(_))));
    }
}
