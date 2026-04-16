// SPDX-License-Identifier: AGPL-3.0-or-later

//! Build script for the `ryve` binary crate.
//!
//! Responsibilities:
//! - Read the pinned tmux version from `vendor/tmux/VERSION` and expose it as
//!   `RYVE_TMUX_VERSION` so `src/bundled_tmux.rs` can embed it at compile time.
//! - Set `RYVE_TMUX_DEV_PATH` to the development-layout path
//!   (`<repo>/vendor/tmux/bin/tmux`) for the dev-build fallback.
//! - Ensure a real tmux binary is present at `RYVE_TMUX_DEV_PATH` on unix
//!   hosts by invoking `scripts/build-vendored-tmux.sh` when it is missing,
//!   so a fresh clone of the repo yields a working `bundled_tmux_path()`
//!   without a separate manual step. See `docs/VENDORED_TMUX.md`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    // Read the pinned tmux version.
    let version_file = manifest_dir.join("vendor/tmux/VERSION");
    let version = std::fs::read_to_string(&version_file)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", version_file.display()));
    let version = version.trim().to_owned();

    println!("cargo:rustc-env=RYVE_TMUX_VERSION={version}");

    // Development-layout path for the bundled tmux binary.
    let dev_path = manifest_dir.join("vendor/tmux/bin/tmux");
    println!("cargo:rustc-env=RYVE_TMUX_DEV_PATH={}", dev_path.display());

    // Rebuild whenever the pinned version or the build script changes so we
    // notice version bumps and script edits. (The absence/presence of the
    // produced binary is handled explicitly below; we do NOT emit a
    // `rerun-if-changed` for `bin/tmux` because cargo would then treat every
    // invocation of the build script — which (re)touches the binary — as a
    // source change and re-run it on every `cargo check`, even when the
    // binary is already up to date.)
    println!("cargo:rerun-if-changed=vendor/tmux/VERSION");
    println!("cargo:rerun-if-changed=scripts/build-vendored-tmux.sh");

    // Respect an opt-out so offline/hermetic builds (e.g. a CI job that
    // stages a pre-built binary into vendor/tmux/bin/tmux) can skip the
    // auto-build entirely.
    let skip = std::env::var("RYVE_SKIP_VENDORED_TMUX_BUILD")
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    println!("cargo:rerun-if-env-changed=RYVE_SKIP_VENDORED_TMUX_BUILD");

    if skip || !is_unix() || dev_path.exists() {
        return;
    }

    build_vendored_tmux(&manifest_dir, &dev_path, &version);
}

#[cfg(unix)]
fn is_unix() -> bool {
    true
}

#[cfg(not(unix))]
fn is_unix() -> bool {
    false
}

/// Invoke `scripts/build-vendored-tmux.sh` to produce `vendor/tmux/bin/tmux`.
///
/// Any failure here is a fatal build error: the rest of the crate assumes
/// `bundled_tmux_path()` resolves to a working executable in the dev layout,
/// and silently continuing would push the failure to `cargo run` time with a
/// far less obvious error ("tmux session not created").
fn build_vendored_tmux(manifest_dir: &Path, dev_path: &Path, version: &str) {
    let script = manifest_dir.join("scripts/build-vendored-tmux.sh");
    if !script.exists() {
        panic!(
            "vendored tmux build script missing at {}; expected it to produce {}",
            script.display(),
            dev_path.display()
        );
    }

    println!(
        "cargo:warning=building vendored tmux {version} via {} (first-time clone; subsequent builds skip this step)",
        script.display()
    );

    let status = Command::new("bash")
        .arg(&script)
        .current_dir(manifest_dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke {}: {e}", script.display()));

    if !status.success() {
        panic!(
            "{} exited with {status}; install the prerequisites listed in docs/VENDORED_TMUX.md \
             (macOS: `brew install autoconf automake libevent pkg-config`) and re-run, or set \
             RYVE_SKIP_VENDORED_TMUX_BUILD=1 to skip auto-build and stage the binary manually",
            script.display()
        );
    }

    if !dev_path.exists() {
        panic!(
            "{} completed successfully but {} is missing — the script's output layout has \
             drifted from build.rs",
            script.display(),
            dev_path.display()
        );
    }
}
