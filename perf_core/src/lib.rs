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

// ── Precomputed git-status/diff maps ────────────────────

/// Build a lookup map that resolves both files (direct) and directories
/// (aggregated) in O(1). For each file entry we also insert aggregated
/// values for every ancestor directory.
///
/// This replaces the per-node O(files) scan that `file_git_status` did
/// for directories on every frame. Spark ryve-252c5b6e.
pub fn precompute_git_status_map(
    statuses: &HashMap<PathBuf, FileStatus>,
) -> HashMap<PathBuf, FileStatus> {
    let mut map: HashMap<PathBuf, FileStatus> = HashMap::with_capacity(statuses.len() * 2);
    for (path, &status) in statuses {
        // File entry — direct.
        map.insert(path.clone(), status);
        // Propagate to every ancestor directory.
        let mut ancestor = path.as_path();
        while let Some(parent) = ancestor.parent() {
            if parent.as_os_str().is_empty() {
                break;
            }
            let entry = map.entry(PathBuf::from(parent)).or_insert(status);
            *entry = higher_priority_status(*entry, status);
            ancestor = parent;
        }
    }
    map
}

/// Build a lookup map that resolves both files (direct) and directories
/// (summed additions/deletions) in O(1). Mirrors
/// [`precompute_git_status_map`] for diff stats.
pub fn precompute_diff_stat_map(
    diff_stats: &HashMap<PathBuf, DiffStat>,
) -> HashMap<PathBuf, DiffStat> {
    let mut map: HashMap<PathBuf, DiffStat> = HashMap::with_capacity(diff_stats.len() * 2);
    for (path, stat) in diff_stats {
        // File entry — direct.
        let file_entry = map.entry(path.clone()).or_default();
        file_entry.additions += stat.additions;
        file_entry.deletions += stat.deletions;
        // Propagate to every ancestor directory.
        let mut ancestor = path.as_path();
        while let Some(parent) = ancestor.parent() {
            if parent.as_os_str().is_empty() {
                break;
            }
            let dir_entry = map.entry(PathBuf::from(parent)).or_default();
            dir_entry.additions += stat.additions;
            dir_entry.deletions += stat.deletions;
            ancestor = parent;
        }
    }
    map
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

// ── Relative-time formatting ────────────────────────────

/// Format an RFC 3339 timestamp as a short relative time string
/// (e.g. "2h ago", "3d ago"). Accepts a pre-computed `now` so callers
/// can share a single clock read across many rows in a frame.
pub fn format_relative_time(rfc3339: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return String::new();
    };
    let duration = now.signed_duration_since(then);

    if duration.num_minutes() < 1 {
        "now".to_string()
    } else if duration.num_minutes() < 60 {
        format!("{}m ago", duration.num_minutes())
    } else if duration.num_hours() < 24 {
        format!("{}h ago", duration.num_hours())
    } else {
        format!("{}d ago", duration.num_days())
    }
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
    fn precompute_git_status_map_aggregates_ancestors() {
        let mut statuses = HashMap::new();
        statuses.insert(PathBuf::from("src/a.rs"), FileStatus::Modified);
        statuses.insert(PathBuf::from("src/b.rs"), FileStatus::Added);
        statuses.insert(PathBuf::from("other/c.rs"), FileStatus::Untracked);

        let map = precompute_git_status_map(&statuses);

        // Files: direct lookup.
        assert_eq!(map.get(Path::new("src/a.rs")), Some(&FileStatus::Modified));
        assert_eq!(map.get(Path::new("src/b.rs")), Some(&FileStatus::Added));
        // Directory: highest priority child (Added > Modified).
        assert_eq!(map.get(Path::new("src")), Some(&FileStatus::Added));
        // Other directory.
        assert_eq!(map.get(Path::new("other")), Some(&FileStatus::Untracked));
    }

    #[test]
    fn precompute_git_status_map_no_sibling_bleed() {
        let mut statuses = HashMap::new();
        statuses.insert(PathBuf::from("src2/foo.rs"), FileStatus::Modified);
        let map = precompute_git_status_map(&statuses);
        // `src` must NOT appear — only `src2` is an ancestor.
        assert_eq!(map.get(Path::new("src")), None);
        assert_eq!(map.get(Path::new("src2")), Some(&FileStatus::Modified));
    }

    #[test]
    fn precompute_diff_stat_map_sums_ancestors() {
        let mut diff_stats = HashMap::new();
        diff_stats.insert(
            PathBuf::from("src/a.rs"),
            DiffStat {
                additions: 5,
                deletions: 2,
            },
        );
        diff_stats.insert(
            PathBuf::from("src/b.rs"),
            DiffStat {
                additions: 3,
                deletions: 1,
            },
        );

        let map = precompute_diff_stat_map(&diff_stats);
        let src = map.get(Path::new("src")).unwrap();
        assert_eq!(src.additions, 8);
        assert_eq!(src.deletions, 3);
    }

    #[test]
    fn format_relative_time_invalid_returns_empty() {
        assert_eq!(format_relative_time("not a date", chrono::Utc::now()), "");
    }

    #[test]
    fn format_relative_time_recent_returns_now() {
        let now = chrono::Utc::now();
        assert_eq!(format_relative_time(&now.to_rfc3339(), now), "now");
    }

    #[test]
    fn format_relative_time_minutes_ago() {
        let now = chrono::Utc::now();
        let then = now - chrono::Duration::minutes(5);
        assert_eq!(format_relative_time(&then.to_rfc3339(), now), "5m ago");
    }

    #[test]
    fn format_relative_time_hours_ago() {
        let now = chrono::Utc::now();
        let then = now - chrono::Duration::hours(3);
        assert_eq!(format_relative_time(&then.to_rfc3339(), now), "3h ago");
    }

    #[test]
    fn format_relative_time_days_ago() {
        let now = chrono::Utc::now();
        let then = now - chrono::Duration::days(2);
        assert_eq!(format_relative_time(&then.to_rfc3339(), now), "2d ago");
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
