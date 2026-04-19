// SPDX-License-Identifier: AGPL-3.0-or-later

//! Smoke tests for the vendored ngIRCd build wiring in `build.rs`. Verifies
//! that the `RYVE_IRCD_VERSION` and `RYVE_IRCD_DEV_PATH` compile-time env
//! vars are set correctly from `vendor/ircd/VERSION` and the expected dev
//! layout. Downstream resolvers (added in a later spark) depend on these
//! vars, so breaking them here would silently break `src/bundled_ircd.rs`.
//!
//! Stamp-file logic (`.version` under the bin dir) is exercised by
//! `tests/vendored_tmux_stamp.rs` against the same shared helpers in
//! `build_vendored_tmux_support.rs` — no need to duplicate that coverage.

use std::path::PathBuf;

#[test]
fn ircd_version_env_matches_pinned_version_file() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pinned = std::fs::read_to_string(manifest_dir.join("vendor/ircd/VERSION"))
        .expect("vendor/ircd/VERSION must exist");

    assert_eq!(
        env!("RYVE_IRCD_VERSION"),
        pinned.trim(),
        "build.rs must export RYVE_IRCD_VERSION matching vendor/ircd/VERSION"
    );
}

#[test]
fn ircd_dev_path_env_points_at_vendor_bin() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expected = manifest_dir.join("vendor/ircd/bin/ngircd");

    assert_eq!(
        PathBuf::from(env!("RYVE_IRCD_DEV_PATH")),
        expected,
        "build.rs must export RYVE_IRCD_DEV_PATH pointing at the dev-layout binary"
    );
}

#[test]
fn ircd_dev_path_parent_is_vendor_ircd_bin() {
    // The stamp-file contract (`vendor/ircd/bin/.version`) hinges on the
    // build output living at `vendor/ircd/bin/`. Lock that layout here so
    // a refactor of `RYVE_IRCD_DEV_PATH` can't silently drift the script's
    // output directory out of sync with build.rs.
    let dev_path = PathBuf::from(env!("RYVE_IRCD_DEV_PATH"));
    let bin_dir = dev_path
        .parent()
        .expect("RYVE_IRCD_DEV_PATH must have a parent directory");
    assert_eq!(bin_dir.file_name().and_then(|s| s.to_str()), Some("bin"));
    assert_eq!(
        bin_dir
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str()),
        Some("ircd")
    );
}
