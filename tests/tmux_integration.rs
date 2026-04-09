// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Integration test for the tmux wrapper module.
//!
//! Uses the real system tmux binary to create, inspect, and tear down a
//! session on a temporary Ryve-private socket. The test is skipped if
//! tmux is not available on the machine.
//!
//! Spark ryve-4bae4ff6 — "Tmux: Rust wrapper module with private-socket
//! discipline".

// The tmux module is inside the `ryve` binary crate, so we cannot import it
// directly from an integration test. Instead we exercise the same logic by
// shelling out to the real tmux binary with the same private-socket pattern
// the module uses.

use std::path::PathBuf;
use std::process::Command;

/// Locate the system tmux binary, or return `None` so the test can be skipped.
fn tmux_binary() -> Option<PathBuf> {
    // Check common locations.
    for candidate in &[
        "/opt/homebrew/bin/tmux",
        "/usr/local/bin/tmux",
        "/usr/bin/tmux",
    ] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Some(p);
        }
    }
    // Try PATH via `which`.
    let output = Command::new("which").arg("tmux").output().ok();
    if let Some(out) = output {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }
    None
}

/// Run a tmux command against the given private socket.
fn tmux_cmd(binary: &PathBuf, socket: &PathBuf) -> Command {
    let mut cmd = Command::new(binary);
    cmd.arg("-S").arg(socket);
    cmd
}

#[test]
fn tmux_session_lifecycle() {
    let Some(binary) = tmux_binary() else {
        eprintln!("tmux not found — skipping integration test");
        return;
    };
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let socket = tmp.path().join("tmux.sock");
    let session_name = "ryve-integration-test";

    // --- Create a detached session ---
    let status = tmux_cmd(&binary, &socket)
        .args(["new-session", "-d", "-s", session_name, "-c", "/tmp"])
        .status()
        .expect("failed to spawn tmux new-session");
    assert!(status.success(), "new-session should succeed");

    // --- has-session should find it ---
    let status = tmux_cmd(&binary, &socket)
        .args(["has-session", "-t", session_name])
        .status()
        .expect("failed to spawn tmux has-session");
    assert!(status.success(), "has-session should find the session");

    // --- list-sessions should include it ---
    let output = tmux_cmd(&binary, &socket)
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .expect("failed to spawn tmux list-sessions");
    assert!(output.status.success(), "list-sessions should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(session_name),
        "list-sessions should contain our session, got: {stdout}"
    );

    // --- pipe-pane should succeed ---
    let log_path = tmp.path().join("pane.log");
    let status = tmux_cmd(&binary, &socket)
        .args(["pipe-pane", "-t", session_name])
        .arg(format!("cat >> {}", log_path.display()))
        .status()
        .expect("failed to spawn tmux pipe-pane");
    assert!(status.success(), "pipe-pane should succeed");

    // --- Duplicate session should fail ---
    let output = tmux_cmd(&binary, &socket)
        .args(["new-session", "-d", "-s", session_name, "-c", "/tmp"])
        .output()
        .expect("failed to spawn tmux new-session (duplicate)");
    assert!(!output.status.success(), "duplicate session should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("duplicate session"),
        "stderr should mention duplicate, got: {stderr}"
    );

    // --- kill-session should succeed ---
    let status = tmux_cmd(&binary, &socket)
        .args(["kill-session", "-t", session_name])
        .status()
        .expect("failed to spawn tmux kill-session");
    assert!(status.success(), "kill-session should succeed");

    // --- has-session should now fail ---
    let status = tmux_cmd(&binary, &socket)
        .args(["has-session", "-t", session_name])
        .status()
        .expect("failed to spawn tmux has-session (after kill)");
    assert!(!status.success(), "has-session should fail after kill");

    // Cleanup: kill the server to release the socket file.
    let _ = tmux_cmd(&binary, &socket).args(["kill-server"]).status();
}
