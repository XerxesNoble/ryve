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

use std::path::{Path, PathBuf};

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
    resolve_bundled_ircd_path_from(
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .as_deref(),
    )
}

/// Resolve the bundled ngIRCd binary given an explicit exe directory,
/// so tests can drive the resolution logic against a controlled layout
/// instead of the real `std::env::current_exe()`. Production callers
/// use [`bundled_ircd_path`], which threads in the real exe dir.
#[cfg(unix)]
fn resolve_bundled_ircd_path_from(exe_dir: Option<&Path>) -> Option<PathBuf> {
    // 1. Installed layout: <exe_dir>/bin/ngircd
    if let Some(dir) = exe_dir {
        let candidate = dir.join("bin").join("ngircd");
        if candidate.exists() {
            return Some(candidate);
        }
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
    fn dev_ircd_path_is_under_vendor() {
        let path = dev_ircd_path();
        // Assert on Path components rather than the string form so the
        // test is path-separator portable (Windows renders with `\`,
        // and while the `unix`-cfg resolver has a stub on non-unix the
        // build.rs sets RYVE_IRCD_DEV_PATH unconditionally).
        let expected_tail = Path::new("vendor").join("ircd").join("bin").join("ngircd");
        assert!(
            path.ends_with(&expected_tail),
            "dev path should end in <vendor>/ircd/bin/ngircd (platform-native \
             separator); expected tail={expected_tail:?}, got path={path:?}"
        );
    }

    /// Exercises `bundled_ircd_path()` against a synthesised "installed
    /// layout" temp dir: `<tmp>/bin/ngircd`. Because the real resolver
    /// keys off `current_exe()`, we drive it via the private
    /// [`resolve_bundled_ircd_path_from`] helper to pass the fake exe
    /// directory. Replaces the prior two tests that asserted on
    /// resolver shape (`exe_relative_path().is_some()` /
    /// `installed_layout_path_structure`) without actually invoking
    /// the resolution logic — those passed regardless of resolver
    /// regressions, which is exactly what Copilot called out.
    #[test]
    fn bundled_ircd_path_prefers_installed_layout_when_present() {
        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let ngircd_path = bin_dir.join("ngircd");
        fs::write(&ngircd_path, "#!/bin/sh\necho fake-ngircd").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&ngircd_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&ngircd_path, perms).unwrap();
        }

        // When the exe-relative path exists, the resolver must return
        // it in preference to the dev path.
        let resolved = resolve_bundled_ircd_path_from(Some(tmp.path()));
        assert_eq!(
            resolved.as_deref(),
            Some(ngircd_path.as_path()),
            "resolver must pick up <exe_dir>/bin/ngircd when it exists",
        );
    }

    /// When nothing is installed at the exe-relative path, the resolver
    /// falls through to the dev path. We can't reliably create the
    /// compile-time `RYVE_IRCD_DEV_PATH` under a TempDir, so we only
    /// assert the fall-through returns the dev path (or None if the
    /// dev path also does not exist in this checkout).
    #[test]
    fn bundled_ircd_path_falls_through_to_dev_when_no_installed_layout() {
        let tmp = TempDir::new().unwrap();
        // Empty tmp dir -> no bin/ngircd -> should fall through to the
        // dev path without ever considering the exe-relative path.
        let resolved = resolve_bundled_ircd_path_from(Some(tmp.path()));
        let dev = dev_ircd_path();
        if dev.exists() {
            assert_eq!(
                resolved.as_deref(),
                Some(dev.as_path()),
                "resolver must fall through to the dev path when installed layout missing",
            );
        } else {
            assert!(
                resolved.is_none(),
                "resolver must return None when neither installed nor dev path exists; \
                 got {resolved:?}",
            );
        }
    }
}
