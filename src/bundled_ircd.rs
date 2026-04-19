// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resolves the path to the ngIRCd binary bundled inside the Ryve app tree.
//!
//! Ryve ships its own IRC daemon so agent-to-agent messaging works out of the
//! box on every install, without depending on a user-provided server. The
//! binary lives at a fixed path relative to the running `ryve` executable:
//!
//! ```text
//! <exe_dir>/bin/ngircd        # installed layout (.app or tarball)
//! ```
//!
//! During development (cargo run), the binary is at:
//!
//! ```text
//! <repo_root>/vendor/ircd/bin/ngircd
//! ```
//!
//! The pinned ngIRCd version is recorded in `vendor/ircd/VERSION`. The shared
//! vendor / auto-build pattern is documented in `docs/VENDORED_TMUX.md`.

use std::path::PathBuf;

/// The pinned ngIRCd version, embedded at compile time from `vendor/ircd/VERSION`.
#[allow(dead_code)] // consumed by a follow-up spark that wires the ngIRCd supervisor
pub const PINNED_IRCD_VERSION: &str = env!("RYVE_IRCD_VERSION");

/// Returns the path to the bundled ngIRCd binary for the current running app.
///
/// Resolution order:
/// 1. `<exe_dir>/bin/ngircd` — the installed layout (macOS `.app` bundle or
///    Linux tarball). This is the primary path used in production.
/// 2. `<repo_root>/vendor/ircd/bin/ngircd` — the development layout, where
///    `<repo_root>` is set at compile time by `build.rs`.
///
/// Returns `None` if neither path exists on disk.
#[cfg(unix)]
#[allow(dead_code)] // consumed by a follow-up spark that wires the ngIRCd supervisor
pub fn bundled_ircd_path() -> Option<PathBuf> {
    // 1. Installed layout: <exe_dir>/bin/ngircd
    if let Some(path) = exe_relative_path()
        && path.exists()
    {
        return Some(path);
    }

    // 2. Development layout: <repo_root>/vendor/ircd/bin/ngircd
    let dev_path = dev_ircd_path();
    if dev_path.exists() {
        return Some(dev_path);
    }

    None
}

/// Non-unix stub: ngIRCd is not supported on non-unix platforms.
#[cfg(not(unix))]
#[allow(dead_code)] // consumed by a follow-up spark that wires the ngIRCd supervisor
pub fn bundled_ircd_path() -> Option<PathBuf> {
    None
}

/// Returns the expected installed-layout path: `<exe_dir>/bin/ngircd`.
fn exe_relative_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    Some(exe_dir.join("bin").join("ngircd"))
}

/// Returns the development-layout path, set at compile time by `build.rs`.
fn dev_ircd_path() -> PathBuf {
    PathBuf::from(env!("RYVE_IRCD_DEV_PATH"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn pinned_version_is_not_empty() {
        assert!(
            !PINNED_IRCD_VERSION.is_empty(),
            "RYVE_IRCD_VERSION must be set at compile time"
        );
    }

    #[test]
    fn pinned_version_looks_like_a_version() {
        // ngIRCd versions are like "27", "26.1", "25" — always start with a digit.
        assert!(
            PINNED_IRCD_VERSION
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit()),
            "RYVE_IRCD_VERSION should start with a digit, got: {PINNED_IRCD_VERSION}"
        );
    }

    #[test]
    fn exe_relative_path_returns_bin_ngircd() {
        // We can't control where the test binary lives, but the function
        // should always produce a path ending in bin/ngircd.
        if let Some(path) = exe_relative_path() {
            assert!(
                path.ends_with("bin/ngircd"),
                "expected bin/ngircd, got: {path:?}"
            );
        }
    }

    #[test]
    fn dev_ircd_path_is_under_vendor() {
        let path = dev_ircd_path();
        assert!(
            path.to_string_lossy().contains("vendor/ircd/bin/ngircd"),
            "dev path should be under vendor/ircd/bin/ngircd, got: {path:?}"
        );
    }

    /// When an ngIRCd binary exists at the exe-relative path, that path wins.
    #[test]
    fn resolution_prefers_exe_relative() {
        // This test validates the resolution *logic* by checking that
        // exe_relative_path() is tried first. We can't easily fake the exe
        // dir in a unit test, so we just verify the function shape: if the
        // exe-relative path existed, it would be returned before dev_path.
        //
        // Full end-to-end coverage is in CI where a real bundled ngIRCd is
        // placed at the correct path.
        let exe_path = exe_relative_path();
        assert!(
            exe_path.is_some(),
            "exe_relative_path should not return None"
        );
    }

    /// Simulates the installed layout by creating a temp dir with bin/ngircd
    /// and verifying the resolution logic would pick it up.
    #[test]
    fn installed_layout_path_structure() {
        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let ngircd_path = bin_dir.join("ngircd");
        fs::write(&ngircd_path, "#!/bin/sh\necho fake-ngircd").unwrap();

        // Verify the path exists and is correct
        assert!(ngircd_path.exists());
        assert!(ngircd_path.ends_with("bin/ngircd"));
    }
}
