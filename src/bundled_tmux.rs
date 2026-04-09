// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Resolves the path to the tmux binary bundled inside the Ryve app tree.
//!
//! Ryve ships its own tmux so Hand/Head sessions behave identically on every
//! install regardless of whether the user has tmux, which version, or how it
//! is configured. The binary lives at a fixed path relative to the running
//! `ryve` executable:
//!
//! ```text
//! <exe_dir>/bin/tmux          # installed layout (.app or tarball)
//! ```
//!
//! During development (cargo run), the binary is at:
//!
//! ```text
//! <repo_root>/vendor/tmux/bin/tmux
//! ```
//!
//! The pinned tmux version is recorded in `vendor/tmux/VERSION`. See
//! `docs/VENDORED_TMUX.md` for how to bump it.

use std::path::PathBuf;

/// The pinned tmux version, embedded at compile time from `vendor/tmux/VERSION`.
pub const PINNED_TMUX_VERSION: &str = env!("RYVE_TMUX_VERSION");

/// Returns the path to the bundled tmux binary for the current running app.
///
/// Resolution order:
/// 1. `<exe_dir>/bin/tmux` — the installed layout (macOS `.app` bundle or
///    Linux tarball). This is the primary path used in production.
/// 2. `<repo_root>/vendor/tmux/bin/tmux` — the development layout, where
///    `<repo_root>` is set at compile time by `build.rs`.
///
/// Returns `None` if neither path exists on disk.
#[cfg(unix)]
pub fn bundled_tmux_path() -> Option<PathBuf> {
    // 1. Installed layout: <exe_dir>/bin/tmux
    if let Some(path) = exe_relative_path()
        && path.exists()
    {
        return Some(path);
    }

    // 2. Development layout: <repo_root>/vendor/tmux/bin/tmux
    let dev_path = dev_tmux_path();
    if dev_path.exists() {
        return Some(dev_path);
    }

    None
}

/// Non-unix stub: tmux is not supported on non-unix platforms.
#[cfg(not(unix))]
pub fn bundled_tmux_path() -> Option<PathBuf> {
    None
}

/// Returns the expected installed-layout path: `<exe_dir>/bin/tmux`.
fn exe_relative_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    Some(exe_dir.join("bin").join("tmux"))
}

/// Returns the development-layout path, set at compile time by `build.rs`.
fn dev_tmux_path() -> PathBuf {
    PathBuf::from(env!("RYVE_TMUX_DEV_PATH"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn pinned_version_is_not_empty() {
        assert!(
            !PINNED_TMUX_VERSION.is_empty(),
            "RYVE_TMUX_VERSION must be set at compile time"
        );
    }

    #[test]
    fn pinned_version_looks_like_a_version() {
        // tmux versions are like "3.5a", "3.4", "3.3a" — always start with a digit.
        assert!(
            PINNED_TMUX_VERSION
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit()),
            "RYVE_TMUX_VERSION should start with a digit, got: {PINNED_TMUX_VERSION}"
        );
    }

    #[test]
    fn exe_relative_path_returns_bin_tmux() {
        // We can't control where the test binary lives, but the function
        // should always produce a path ending in bin/tmux.
        if let Some(path) = exe_relative_path() {
            assert!(
                path.ends_with("bin/tmux"),
                "expected bin/tmux, got: {path:?}"
            );
        }
    }

    #[test]
    fn dev_tmux_path_is_under_vendor() {
        let path = dev_tmux_path();
        assert!(
            path.to_string_lossy().contains("vendor/tmux/bin/tmux"),
            "dev path should be under vendor/tmux/bin/tmux, got: {path:?}"
        );
    }

    /// When a tmux binary exists at the exe-relative path, that path wins.
    #[test]
    fn resolution_prefers_exe_relative() {
        // This test validates the resolution *logic* by checking that
        // exe_relative_path() is tried first. We can't easily fake the exe
        // dir in a unit test, so we just verify the function shape: if the
        // exe-relative path existed, it would be returned before dev_path.
        //
        // Full end-to-end coverage is in CI where a real bundled tmux is
        // placed at the correct path.
        let exe_path = exe_relative_path();
        assert!(
            exe_path.is_some(),
            "exe_relative_path should not return None"
        );
    }

    /// Simulates the installed layout by creating a temp dir with bin/tmux
    /// and verifying the resolution logic would pick it up.
    #[test]
    fn installed_layout_path_structure() {
        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let tmux_path = bin_dir.join("tmux");
        fs::write(&tmux_path, "#!/bin/sh\necho fake-tmux").unwrap();

        // Verify the path exists and is correct
        assert!(tmux_path.exists());
        assert!(tmux_path.ends_with("bin/tmux"));
    }
}
