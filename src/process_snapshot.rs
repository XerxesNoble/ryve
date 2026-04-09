// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! A single shared `sysinfo` snapshot for the UI's per-tick liveness checks.
//!
//! Spark `ryve-a5b9e4a1`: previously every persisted agent session and every
//! untracked terminal triggered its own `System::new_all()` + `refresh_processes`
//! on the UI thread inside `update()`. With N sessions across K workshops that
//! turned each `SparksPoll` tick into N*K full /proc scans, every 3 seconds.
//!
//! This module captures one snapshot per tick (off the UI thread, via
//! `tokio::task::spawn_blocking`) and exposes:
//!
//! - [`ProcessSnapshot::is_alive`] for cheap PID liveness checks
//! - [`ProcessSnapshot::detect_agent_in_tree`] for the auto-detect path that
//!   walks a shell's process tree looking for a known coding agent

use std::collections::{HashMap, HashSet};

use sysinfo::{ProcessesToUpdate, System};

use crate::coding_agents::{CodingAgent, CompatStatus, ResumeStrategy};

/// A frozen view of the OS process table at one point in time.
#[derive(Debug, Default, Clone)]
pub struct ProcessSnapshot {
    /// Set of every PID present at capture time.
    live_pids: HashSet<u32>,
    /// `pid -> (parent_pid, executable_name)`. Used by [`detect_agent_in_tree`]
    /// to walk a shell's descendants without re-scanning the system.
    info: HashMap<u32, (Option<u32>, String)>,
}

impl ProcessSnapshot {
    /// Build a fresh snapshot. **Blocking** — call from `spawn_blocking`,
    /// never directly from the UI thread.
    pub fn capture() -> Self {
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let processes = sys.processes();
        let mut live_pids = HashSet::with_capacity(processes.len());
        let mut info = HashMap::with_capacity(processes.len());
        for (pid, proc_info) in processes {
            let p = pid.as_u32();
            live_pids.insert(p);
            info.insert(
                p,
                (
                    proc_info.parent().map(|pp| pp.as_u32()),
                    proc_info.name().to_string_lossy().into_owned(),
                ),
            );
        }
        Self { live_pids, info }
    }

    /// True if the process was present at capture time.
    pub fn is_alive(&self, child_pid: i64) -> bool {
        u32::try_from(child_pid)
            .map(|p| self.live_pids.contains(&p))
            .unwrap_or(false)
    }

    /// Walk descendants of `shell_pid` looking for a known coding-agent
    /// binary. Returns the matching [`CodingAgent`] or `None` if no
    /// descendant matches.
    pub fn detect_agent_in_tree(&self, shell_pid: u32) -> Option<CodingAgent> {
        // Known agent binary names → CodingAgent constructors. Keep in sync
        // with `coding_agents::detect_available`.
        const KNOWN: &[(&str, &str, ResumeStrategy)] = &[
            ("claude", "Claude Code", ResumeStrategy::ResumeFlag),
            ("codex", "Codex", ResumeStrategy::ResumeFlag),
            ("aider", "Aider", ResumeStrategy::None),
            ("opencode", "OpenCode", ResumeStrategy::None),
        ];

        let mut queue = vec![shell_pid];
        let mut visited: HashSet<u32> = HashSet::new();
        visited.insert(shell_pid);

        while let Some(parent) = queue.pop() {
            for (&child, (child_parent, name)) in &self.info {
                if *child_parent != Some(parent) {
                    continue;
                }
                if !visited.insert(child) {
                    continue;
                }
                for &(cmd, display, ref resume) in KNOWN {
                    if name == cmd {
                        return Some(CodingAgent {
                            display_name: display.to_string(),
                            command: cmd.to_string(),
                            args: Vec::new(),
                            resume: resume.clone(),
                            compatibility: CompatStatus::Unknown,
                        });
                    }
                }
                queue.push(child);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reports_self_as_alive() {
        let snap = ProcessSnapshot::capture();
        let me = std::process::id() as i64;
        assert!(snap.is_alive(me), "current pid {me} should be in snapshot");
    }

    #[test]
    fn snapshot_handles_negative_and_oversized_pids() {
        let snap = ProcessSnapshot::capture();
        assert!(!snap.is_alive(-1));
        assert!(!snap.is_alive(i64::from(u32::MAX) + 1));
    }

    #[test]
    fn snapshot_dead_pid_is_not_alive() {
        // PID 0 is reserved on every supported OS and never appears in
        // /proc-style tables, so this is a stable "definitely not alive"
        // check that doesn't depend on which other processes happen to be
        // running on the test host.
        let snap = ProcessSnapshot::capture();
        assert!(!snap.is_alive(0));
    }

    #[test]
    fn detect_agent_returns_none_for_unknown_root() {
        let snap = ProcessSnapshot::capture();
        // u32::MAX is overwhelmingly unlikely to be a real PID; the BFS
        // should terminate cleanly without finding anything.
        assert!(snap.detect_agent_in_tree(u32::MAX).is_none());
    }
}
