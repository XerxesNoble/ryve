// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Pure, benchmarkable helpers extracted from Ryve's hottest UI paths.
//!
//! This crate exists for one reason: to be the *single* home of the small
//! pure functions that the performance regression harness measures. Every
//! function here is also called from the live binary so the benchmarks
//! reflect what users actually pay for.
//!
//! Spark `ryve-5b9c5d93` — Performance regression harness.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use data::git::{DiffStat, FileStatus};
use sysinfo::{Pid, ProcessesToUpdate, System};

// ── Process liveness ─────────────────────────────────────

/// Return true if a process with the given PID is currently alive.
///
/// Builds a fresh `System` view on every call — that's the cost the binary
/// pays today, and the regression harness measures exactly that shape.
pub fn process_is_alive(pid: u32) -> bool {
    let mut system = System::new_all();
    system.refresh_processes(ProcessesToUpdate::All, true);
    system.process(Pid::from_u32(pid)).is_some()
}

/// Same as [`process_is_alive`] but accepts the signed pid representation
/// the binary stores in SQLite.
pub fn process_is_alive_i64(child_pid: i64) -> bool {
    let Ok(pid) = u32::try_from(child_pid) else {
        return false;
    };
    process_is_alive(pid)
}

// ── Tree node kind ───────────────────────────────────────

/// Mirror of `screen::file_explorer::NodeKind`. Lives here so the file
/// explorer can call into the shared aggregation routine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Directory,
}

// ── Git-status aggregation ───────────────────────────────

/// Resolve the effective git status for a file or directory.
///
/// For files this is a single hash lookup. For directories it scans every
/// status entry and picks the highest-priority descendant. This is the hot
/// path the file explorer hits on every redraw — keep it pure and tight.
pub fn file_git_status(
    rel_path: &Path,
    kind: NodeKind,
    statuses: &HashMap<PathBuf, FileStatus>,
) -> Option<FileStatus> {
    if kind == NodeKind::File {
        return statuses.get(rel_path).copied();
    }

    let mut most_important: Option<FileStatus> = None;
    for (path, status) in statuses {
        if path == rel_path {
            continue;
        }
        if path.starts_with(rel_path) {
            most_important = Some(match most_important {
                None => *status,
                Some(prev) => higher_priority_status(prev, *status),
            });
        }
    }
    most_important
}

/// Aggregate diff stats over a directory's strict descendants. File entries
/// short-circuit to a direct lookup.
pub fn file_diff_stat(
    rel_path: &Path,
    kind: NodeKind,
    diff_stats: &HashMap<PathBuf, DiffStat>,
) -> DiffStat {
    if kind == NodeKind::File {
        return diff_stats.get(rel_path).copied().unwrap_or_default();
    }

    let mut total = DiffStat::default();
    for (path, stat) in diff_stats {
        if path == rel_path {
            continue;
        }
        if path.starts_with(rel_path) {
            total.additions += stat.additions;
            total.deletions += stat.deletions;
        }
    }
    total
}

fn higher_priority_status(a: FileStatus, b: FileStatus) -> FileStatus {
    fn rank(s: FileStatus) -> u8 {
        match s {
            FileStatus::Conflicted => 7,
            FileStatus::Deleted => 6,
            FileStatus::Added => 5,
            FileStatus::Modified => 4,
            FileStatus::Renamed => 3,
            FileStatus::Copied => 2,
            FileStatus::Untracked => 1,
            FileStatus::Ignored => 0,
        }
    }
    if rank(b) > rank(a) { b } else { a }
}

// ── Session filter ───────────────────────────────────────

/// Trait to abstract over the agent-session shape the binary uses without
/// pulling the whole binary into perf_core.
pub trait SessionLike {
    fn is_active(&self) -> bool;
    fn is_stale(&self) -> bool;
}

/// Count the number of `active && !stale` sessions in a slice. This shape
/// fires every time the agent panel re-renders.
pub fn count_active_sessions<S: SessionLike>(sessions: &[S]) -> usize {
    sessions
        .iter()
        .filter(|s| s.is_active() && !s.is_stale())
        .count()
}

// ── Key-event dispatch classifier ────────────────────────

/// Logical kind of a synthetic key press, independent of any UI framework.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Character(char),
    Escape,
    ModifiersChanged { shift: bool },
    Other,
}

/// Modifier state at the moment of the key press.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyModifiers {
    pub command: bool,
}

/// What the global keyboard subscription should dispatch when it sees a
/// given event. Anything that does not map to a real hotkey collapses to
/// [`KeyDispatch::Noop`] — *not* `SparksPoll`.
///
/// Routing every unmatched key through `SparksPoll` was the bug the perf
/// regression test in `tests/sparks_poll_smoke.rs` exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDispatch {
    NewDefaultHand,
    CopySelection,
    HotkeyCmdF,
    NewWorkshopDialog,
    HotkeyEscape,
    ShiftStateChanged(bool),
    Noop,
    /// Reserved: should never be returned by [`classify_key_event`].
    /// The smoke test asserts a synthetic key burst yields zero of these.
    SparksPoll,
}

/// Pure version of the global keyboard hotkey routing in `App::subscription`.
///
/// The binary's `subscription()` builds the same dispatch table on top of
/// Iced's keyboard event types; this function is the part the regression
/// harness can drive without booting the GUI.
pub fn classify_key_event(kind: KeyKind, modifiers: KeyModifiers) -> KeyDispatch {
    match kind {
        KeyKind::Character(c) if modifiers.command => match c {
            'h' => KeyDispatch::NewDefaultHand,
            'c' => KeyDispatch::CopySelection,
            'f' => KeyDispatch::HotkeyCmdF,
            'o' => KeyDispatch::NewWorkshopDialog,
            _ => KeyDispatch::Noop,
        },
        KeyKind::Character(_) => KeyDispatch::Noop,
        KeyKind::Escape => KeyDispatch::HotkeyEscape,
        KeyKind::ModifiersChanged { shift } => KeyDispatch::ShiftStateChanged(shift),
        KeyKind::Other => KeyDispatch::Noop,
    }
}

// ── Log-tail virtualization ──────────────────────────────

/// Estimated height in logical pixels of a single monospace line at body
/// font size. Shared between the UI and the benchmark.
pub const LOG_LINE_HEIGHT: f32 = 20.0;

/// Extra lines rendered above/below the viewport to avoid flicker.
pub const LOG_OVERSCAN_LINES: usize = 5;

/// Given a scroll offset, viewport height, and total line count, return the
/// `(first, last)` indices of lines to render (half-open range). The range
/// includes overscan margins so scrolling doesn't flash empty space.
///
/// This is the pure hot-path that determines which log lines the UI
/// materialises on each frame.
pub fn log_tail_visible_range(
    offset_y: f32,
    viewport_height: f32,
    total_lines: usize,
) -> (usize, usize) {
    if total_lines == 0 {
        return (0, 0);
    }
    let first = (offset_y / LOG_LINE_HEIGHT).floor() as usize;
    let visible_count = (viewport_height / LOG_LINE_HEIGHT).ceil() as usize;
    let first = first.saturating_sub(LOG_OVERSCAN_LINES).min(total_lines);
    let last = (first + visible_count + 2 * LOG_OVERSCAN_LINES).min(total_lines);
    (first, last)
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_git_status_direct_file() {
        let mut statuses = HashMap::new();
        statuses.insert(PathBuf::from("src/main.rs"), FileStatus::Modified);
        let got = file_git_status(Path::new("src/main.rs"), NodeKind::File, &statuses);
        assert_eq!(got, Some(FileStatus::Modified));
    }

    #[test]
    fn file_git_status_directory_aggregates_children() {
        let mut statuses = HashMap::new();
        statuses.insert(PathBuf::from("src/a.rs"), FileStatus::Modified);
        statuses.insert(PathBuf::from("src/b.rs"), FileStatus::Added);
        let got = file_git_status(Path::new("src"), NodeKind::Directory, &statuses);
        assert_eq!(got, Some(FileStatus::Added));
    }

    #[test]
    fn file_git_status_directory_does_not_match_sibling_with_same_prefix() {
        let mut statuses = HashMap::new();
        statuses.insert(PathBuf::from("src2/foo.rs"), FileStatus::Modified);
        let got = file_git_status(Path::new("src"), NodeKind::Directory, &statuses);
        assert_eq!(got, None);
    }

    #[test]
    fn classify_unmatched_chars_are_noop_not_sparks_poll() {
        for c in 'a'..='z' {
            let out = classify_key_event(KeyKind::Character(c), KeyModifiers::default());
            assert_eq!(
                out,
                KeyDispatch::Noop,
                "key {c} must be Noop, not SparksPoll"
            );
            assert_ne!(out, KeyDispatch::SparksPoll);
        }
    }

    #[test]
    fn classify_known_hotkeys() {
        let cmd = KeyModifiers { command: true };
        assert_eq!(
            classify_key_event(KeyKind::Character('h'), cmd),
            KeyDispatch::NewDefaultHand
        );
        assert_eq!(
            classify_key_event(KeyKind::Character('o'), cmd),
            KeyDispatch::NewWorkshopDialog
        );
        assert_eq!(
            classify_key_event(KeyKind::Escape, KeyModifiers::default()),
            KeyDispatch::HotkeyEscape
        );
    }

    #[derive(Debug)]
    struct FakeSession {
        active: bool,
        stale: bool,
    }
    impl SessionLike for FakeSession {
        fn is_active(&self) -> bool {
            self.active
        }
        fn is_stale(&self) -> bool {
            self.stale
        }
    }

    #[test]
    fn count_active_sessions_excludes_stale() {
        let sessions = vec![
            FakeSession {
                active: true,
                stale: false,
            },
            FakeSession {
                active: true,
                stale: true,
            },
            FakeSession {
                active: false,
                stale: false,
            },
        ];
        assert_eq!(count_active_sessions(&sessions), 1);
    }
}
