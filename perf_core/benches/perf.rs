// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Performance regression benchmarks for the hottest pure functions in
//! Ryve's UI hot loop. Output is consumed by `scripts/perf-gate.py` and
//! gated against `perf-budgets.toml` in CI.
//!
//! Spark `ryve-5b9c5d93`.

use std::collections::HashMap;
use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use data::git::{DiffStat, FileStatus};
use perf_core::{
    KeyDispatch, KeyKind, KeyModifiers, NodeKind, SessionLike, classify_key_event,
    count_active_sessions, file_git_status, precompute_diff_stat_map, precompute_git_status_map,
    process_is_alive,
};

// ── Fixtures ─────────────────────────────────────────────

fn make_status_map(n: usize) -> HashMap<PathBuf, FileStatus> {
    // Synthetic repo: a flat src/ directory with `n` modified files plus
    // a few unrelated siblings to exercise the prefix-match branch.
    let mut m = HashMap::with_capacity(n + 4);
    for i in 0..n {
        m.insert(
            PathBuf::from(format!("src/module_{i:04}/file_{i:04}.rs")),
            if i.is_multiple_of(3) {
                FileStatus::Modified
            } else if i.is_multiple_of(5) {
                FileStatus::Added
            } else {
                FileStatus::Untracked
            },
        );
    }
    m.insert(PathBuf::from("docs/README.md"), FileStatus::Modified);
    m.insert(PathBuf::from("src2/foo.rs"), FileStatus::Modified);
    m.insert(PathBuf::from("Cargo.toml"), FileStatus::Modified);
    m
}

#[derive(Debug, Clone)]
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

fn make_sessions(n: usize) -> Vec<FakeSession> {
    (0..n)
        .map(|i| FakeSession {
            active: i.is_multiple_of(2),
            stale: i.is_multiple_of(7),
        })
        .collect()
}

fn make_diff_stat_map(n: usize) -> HashMap<PathBuf, DiffStat> {
    let mut m = HashMap::with_capacity(n);
    for i in 0..n {
        m.insert(
            PathBuf::from(format!("src/module_{i:04}/file_{i:04}.rs")),
            DiffStat {
                additions: (i as u32 % 20) + 1,
                deletions: (i as u32 % 10),
            },
        );
    }
    m
}

// ── Benchmarks ───────────────────────────────────────────

fn bench_process_is_alive(c: &mut Criterion) {
    // Always check the current process — guaranteed to exist on every OS.
    let pid = std::process::id();
    c.bench_function("process_is_alive", |b| {
        b.iter(|| {
            let alive = process_is_alive(std::hint::black_box(pid));
            std::hint::black_box(alive);
        });
    });
}

fn bench_session_filter(c: &mut Criterion) {
    let sessions = make_sessions(256);
    c.bench_function("session_filter", |b| {
        b.iter(|| {
            let n = count_active_sessions(std::hint::black_box(&sessions));
            std::hint::black_box(n);
        });
    });
}

fn bench_file_git_status(c: &mut Criterion) {
    let statuses = make_status_map(512);
    let dir = PathBuf::from("src");
    c.bench_function("file_git_status_dir_aggregation", |b| {
        b.iter(|| {
            let s = file_git_status(
                std::hint::black_box(&dir),
                NodeKind::Directory,
                std::hint::black_box(&statuses),
            );
            std::hint::black_box(s);
        });
    });
}

fn bench_classify_key_event(c: &mut Criterion) {
    // Stand-in for the agent_context::sync no-op path: a tiny pure function
    // called on every keystroke. Cheap to measure, immediate feedback if a
    // future change makes the dispatcher allocate or branch heavily.
    let key = KeyKind::Character('z');
    let mods = KeyModifiers::default();
    c.bench_function("classify_key_event_unmatched", |b| {
        b.iter(|| {
            let out = classify_key_event(std::hint::black_box(key), std::hint::black_box(mods));
            assert_eq!(out, KeyDispatch::Noop);
        });
    });
}

// ── Precomputed map benchmarks (spark ryve-252c5b6e) ────

fn bench_precompute_git_status_map(c: &mut Criterion) {
    let statuses = make_status_map(512);
    c.bench_function("precompute_git_status_map_512", |b| {
        b.iter(|| {
            let map = precompute_git_status_map(std::hint::black_box(&statuses));
            std::hint::black_box(map);
        });
    });
}

fn bench_precompute_diff_stat_map(c: &mut Criterion) {
    let diff_stats = make_diff_stat_map(512);
    c.bench_function("precompute_diff_stat_map_512", |b| {
        b.iter(|| {
            let map = precompute_diff_stat_map(std::hint::black_box(&diff_stats));
            std::hint::black_box(map);
        });
    });
}

/// Compare the old per-node directory scan (`file_git_status` called N
/// times) against a single precomputed map lookup. The difference shows
/// the improvement `view_workshop` gets from caching. Spark ryve-252c5b6e.
fn bench_view_workshop_status_lookups(c: &mut Criterion) {
    let statuses = make_status_map(512);
    let precomputed = precompute_git_status_map(&statuses);
    let dirs: Vec<PathBuf> = (0..50)
        .map(|i| PathBuf::from(format!("src/module_{i:04}")))
        .collect();

    // Old path: O(files) per directory lookup.
    c.bench_function("view_workshop_old_dir_scan_50x512", |b| {
        b.iter(|| {
            for dir in &dirs {
                let s = file_git_status(
                    std::hint::black_box(dir),
                    NodeKind::Directory,
                    std::hint::black_box(&statuses),
                );
                std::hint::black_box(s);
            }
        });
    });

    // New path: O(1) lookup from precomputed map.
    c.bench_function("view_workshop_new_map_lookup_50", |b| {
        b.iter(|| {
            for dir in &dirs {
                let s = precomputed.get(std::hint::black_box(dir));
                std::hint::black_box(s);
            }
        });
    });
}

criterion_group!(
    benches,
    bench_process_is_alive,
    bench_session_filter,
    bench_file_git_status,
    bench_classify_key_event,
    bench_precompute_git_status_map,
    bench_precompute_diff_stat_map,
    bench_view_workshop_status_lookups,
);
criterion_main!(benches);
