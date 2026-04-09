// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Thin tmux session wrapper for Ryve agent-session liveness.
//!
//! Spark `ryve-a677498c`: the workshop poll loop needs a `list_sessions`
//! abstraction to diff live sessions against tracked `agent_session` rows.
//! Currently delegates to [`ProcessSnapshot`] for liveness detection — every
//! Hand/Head stores its `child_pid` in the DB and we check whether that PID
//! is still present in the OS process table.
//!
//! When the bundled-tmux integration lands (parent epic `ryve-0285181c` /
//! spark `ryve-4bae4ff6`), this module will be replaced with real `tmux
//! list-sessions` calls against the Ryve-private socket.

use crate::process_snapshot::ProcessSnapshot;

/// Liveness summary for a single tracked agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// The `agent_sessions.id` this entry corresponds to.
    pub session_id: String,
    /// Whether the session's process is still running.
    pub alive: bool,
}

/// Query liveness for all tracked agent sessions by diffing their stored
/// `child_pid` values against a [`ProcessSnapshot`].
///
/// `tracked` is a slice of `(session_id, child_pid)` pairs drawn from the
/// `agent_sessions` table. Sessions without a `child_pid` are reported as
/// dead — there is no process to check.
///
/// This function is intentionally cheap: it performs no syscalls, only
/// hash-set lookups inside the already-captured snapshot, so it is safe to
/// call on every poll tick without added jank.
pub fn list_sessions(
    tracked: &[(String, Option<i64>)],
    snapshot: &ProcessSnapshot,
) -> Vec<SessionInfo> {
    tracked
        .iter()
        .map(|(id, pid)| {
            let alive = pid.map(|p| snapshot.is_alive(p)).unwrap_or(false);
            SessionInfo {
                session_id: id.clone(),
                alive,
            }
        })
        .collect()
}

/// Diff tracked sessions against the snapshot and return only session IDs
/// whose process has disappeared — i.e. sessions that were expected to be
/// alive but are no longer present in the process table.
///
/// This is the reconciliation entry-point: every returned session ID needs
/// its `agent_sessions` row ended and its active `hand_assignments` rows
/// transitioned to a terminal state.
pub fn dead_sessions(tracked: &[(String, Option<i64>)], snapshot: &ProcessSnapshot) -> Vec<String> {
    list_sessions(tracked, snapshot)
        .into_iter()
        .filter(|s| !s.alive)
        .map(|s| s.session_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A snapshot with no processes — every PID reports dead.
    fn empty_snapshot() -> ProcessSnapshot {
        ProcessSnapshot::default()
    }

    #[test]
    fn session_with_no_pid_is_dead() {
        let tracked = vec![("sess-1".to_string(), None)];
        let results = list_sessions(&tracked, &empty_snapshot());
        assert_eq!(results.len(), 1);
        assert!(!results[0].alive);
    }

    #[test]
    fn session_with_missing_pid_is_dead() {
        let tracked = vec![("sess-2".to_string(), Some(99999))];
        let results = list_sessions(&tracked, &empty_snapshot());
        assert!(!results[0].alive);
    }

    #[test]
    fn session_with_live_pid_is_alive() {
        let snapshot = ProcessSnapshot::capture();
        let my_pid = std::process::id() as i64;
        let tracked = vec![("sess-3".to_string(), Some(my_pid))];
        let results = list_sessions(&tracked, &snapshot);
        assert!(results[0].alive);
    }

    #[test]
    fn dead_sessions_filters_to_dead_only() {
        let snapshot = ProcessSnapshot::capture();
        let my_pid = std::process::id() as i64;
        let tracked = vec![
            ("alive".to_string(), Some(my_pid)),
            ("dead".to_string(), Some(99999)),
            ("no-pid".to_string(), None),
        ];
        let dead = dead_sessions(&tracked, &snapshot);
        assert_eq!(dead.len(), 2);
        assert!(dead.contains(&"dead".to_string()));
        assert!(dead.contains(&"no-pid".to_string()));
        assert!(!dead.contains(&"alive".to_string()));
    }
}
