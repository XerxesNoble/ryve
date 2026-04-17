// SPDX-License-Identifier: AGPL-3.0-or-later

//! Unit tests for the stamp-file helpers shared with `build.rs`. Exercises
//! the logic that decides whether `scripts/build-vendored-tmux.sh` needs to
//! re-run because `vendor/tmux/VERSION` has changed since the last build.
//!
//! The helpers are `#[path]`-included from the repo root so both the build
//! script and this test binary compile against the same source.

#[path = "../build_vendored_tmux_support.rs"]
mod support;

use tempfile::TempDir;

#[test]
fn stamp_matches_is_false_when_stamp_is_missing() {
    let tmp = TempDir::new().unwrap();
    let stamp = support::stamp_path(tmp.path());

    assert!(
        !support::stamp_matches(&stamp, "3.5a"),
        "missing stamp must force a rebuild"
    );
    assert_eq!(support::read_stamp(&stamp), None);
}

#[test]
fn stamp_matches_is_true_after_write() {
    let tmp = TempDir::new().unwrap();
    let stamp = support::stamp_path(tmp.path());

    support::write_stamp(&stamp, "3.5a").unwrap();

    assert!(support::stamp_matches(&stamp, "3.5a"));
    assert_eq!(support::read_stamp(&stamp).as_deref(), Some("3.5a"));
}

#[test]
fn stamp_matches_is_false_on_version_mismatch() {
    let tmp = TempDir::new().unwrap();
    let stamp = support::stamp_path(tmp.path());

    support::write_stamp(&stamp, "3.5a").unwrap();

    assert!(
        !support::stamp_matches(&stamp, "3.6"),
        "a bumped VERSION must trigger a rebuild even when the binary is on disk"
    );
}

#[test]
fn stamp_matches_ignores_surrounding_whitespace() {
    let tmp = TempDir::new().unwrap();
    let stamp = support::stamp_path(tmp.path());

    // Simulate both a shell `printf '%s\n'` write and a human-edited file
    // with trailing blank lines / leading spaces.
    std::fs::write(&stamp, "  3.5a\n\n").unwrap();

    assert!(support::stamp_matches(&stamp, "3.5a"));
    assert!(support::stamp_matches(&stamp, "  3.5a  "));
}

#[test]
fn write_stamp_creates_missing_parent_directory() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("vendor/tmux/bin");
    let stamp = support::stamp_path(&bin_dir);

    assert!(
        !bin_dir.exists(),
        "precondition: bin dir does not exist yet"
    );

    support::write_stamp(&stamp, "3.5a").unwrap();

    assert!(bin_dir.is_dir(), "write_stamp must create parent dirs");
    assert!(support::stamp_matches(&stamp, "3.5a"));
}

#[test]
fn stamp_path_is_dot_version_under_bin_dir() {
    let bin_dir = std::path::Path::new("/tmp/vendor/tmux/bin");
    let stamp = support::stamp_path(bin_dir);

    assert_eq!(stamp, bin_dir.join(".version"));
}
