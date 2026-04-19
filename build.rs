// SPDX-License-Identifier: AGPL-3.0-or-later

//! Build script for the `ryve` binary crate.
//!
//! Responsibilities:
//! - Read the pinned tmux version from `vendor/tmux/VERSION` and expose it as
//!   `RYVE_TMUX_VERSION` so `src/bundled_tmux.rs` can embed it at compile time.
//! - Set `RYVE_TMUX_DEV_PATH` to the development-layout path
//!   (`<repo>/vendor/tmux/bin/tmux`) for the dev-build fallback.
//! - Do the same for the vendored IRC daemon: read `vendor/ircd/VERSION`,
//!   export `RYVE_IRCD_VERSION` + `RYVE_IRCD_DEV_PATH`, and auto-build via
//!   `scripts/build-vendored-ircd.sh`. See `docs/VENDORED_TMUX.md` for the
//!   shared hermetic / opt-out / stamp-file pattern.
//! - Ensure a real tmux binary is present at `RYVE_TMUX_DEV_PATH` on unix
//!   hosts by invoking `scripts/build-vendored-tmux.sh` when it is missing
//!   OR when `vendor/tmux/VERSION` has changed since the last successful
//!   build (detected via the `.version` stamp file the script writes into
//!   `vendor/tmux/bin/`). The IRC daemon uses the same pattern against
//!   `vendor/ircd/bin/.version`.
//!
//! If the native build prerequisites (libevent / ncurses dev headers for
//! tmux, or a C compiler / make for ngIRCd) are missing — typical on
//! minimal Linux CI images that only run `cargo check` or `cargo clippy` —
//! we emit a `cargo:warning` and skip the auto-build rather than failing
//! the compile. Downstream callers that actually need the binary gate on
//! `bundled_tmux_path()` / `bundled_ircd_path()` returning `Some(...)`.

#[path = "build_vendored_tmux_support.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    setup_vendored_tmux(&manifest_dir);
    setup_vendored_ircd(&manifest_dir);
}

fn setup_vendored_tmux(manifest_dir: &Path) {
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
    // notice version bumps and script edits. (The presence of the produced
    // binary is handled explicitly below; we do NOT emit a `rerun-if-changed`
    // for `bin/tmux` because cargo would then treat every invocation of the
    // build script — which (re)touches the binary — as a source change and
    // re-run it on every `cargo check`, even when the binary is already up
    // to date.)
    println!("cargo:rerun-if-changed=vendor/tmux/VERSION");
    println!("cargo:rerun-if-changed=scripts/build-vendored-tmux.sh");

    // Respect an opt-out so offline/hermetic builds (e.g. a CI job that
    // stages a pre-built binary into vendor/tmux/bin/tmux) can skip the
    // auto-build entirely.
    let skip = std::env::var("RYVE_SKIP_VENDORED_TMUX_BUILD")
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    println!("cargo:rerun-if-env-changed=RYVE_SKIP_VENDORED_TMUX_BUILD");

    if skip || !is_unix() {
        return;
    }

    let bin_dir = dev_path.parent().expect("dev tmux path must have a parent");
    let stamp = support::stamp_path(bin_dir);
    if dev_path.exists() && support::stamp_matches(&stamp, &version) {
        return;
    }

    if !has_tmux_build_deps() {
        println!(
            "cargo:warning=vendored tmux build skipped: libevent/ncurses development headers \
             not found via pkg-config. Install the prerequisites from docs/VENDORED_TMUX.md \
             (Linux: apt-get install libevent-dev libncurses-dev pkg-config) and re-run, or \
             set RYVE_SKIP_VENDORED_TMUX_BUILD=1 to silence this warning. Code paths that \
             need tmux will fall back via bundled_tmux_path()."
        );
        return;
    }

    build_vendored_tmux(manifest_dir, &dev_path, &version);

    // Defensive: the build script writes the stamp itself, but write it
    // again here so a successful run always leaves a valid stamp even if a
    // future script refactor forgets. Intentional duplication — not churn.
    if let Err(e) = support::write_stamp(&stamp, &version) {
        println!(
            "cargo:warning=failed to write vendored tmux stamp {}: {e}",
            stamp.display()
        );
    }
}

fn setup_vendored_ircd(manifest_dir: &Path) {
    // Read the pinned ngIRCd version.
    let version_file = manifest_dir.join("vendor/ircd/VERSION");
    let version = std::fs::read_to_string(&version_file)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", version_file.display()));
    let version = version.trim().to_owned();

    println!("cargo:rustc-env=RYVE_IRCD_VERSION={version}");

    // Development-layout path for the bundled ngIRCd binary.
    let dev_path = manifest_dir.join("vendor/ircd/bin/ngircd");
    println!("cargo:rustc-env=RYVE_IRCD_DEV_PATH={}", dev_path.display());

    println!("cargo:rerun-if-changed=vendor/ircd/VERSION");
    println!("cargo:rerun-if-changed=scripts/build-vendored-ircd.sh");

    let skip = std::env::var("RYVE_SKIP_VENDORED_IRCD_BUILD")
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    println!("cargo:rerun-if-env-changed=RYVE_SKIP_VENDORED_IRCD_BUILD");

    if skip || !is_unix() {
        return;
    }

    let bin_dir = dev_path
        .parent()
        .expect("dev ngircd path must have a parent");
    let stamp = support::stamp_path(bin_dir);
    if dev_path.exists() && support::stamp_matches(&stamp, &version) {
        return;
    }

    if !has_ircd_build_deps() {
        println!(
            "cargo:warning=vendored ngIRCd build skipped: a C compiler (cc) and make were not \
             found on PATH. Install the prerequisites (macOS: xcode-select --install; Linux: \
             apt-get install build-essential) and re-run, or set \
             RYVE_SKIP_VENDORED_IRCD_BUILD=1 to silence this warning. Code paths that need \
             ngIRCd will fall back via bundled_ircd_path()."
        );
        return;
    }

    build_vendored_ircd(manifest_dir, &dev_path, &version);

    // Defensive: mirror the tmux path — the script writes the stamp, but we
    // re-write here so a successful build.rs run always leaves a valid stamp.
    if let Err(e) = support::write_stamp(&stamp, &version) {
        println!(
            "cargo:warning=failed to write vendored ngIRCd stamp {}: {e}",
            stamp.display()
        );
    }
}

#[cfg(unix)]
fn is_unix() -> bool {
    true
}

#[cfg(not(unix))]
fn is_unix() -> bool {
    false
}

/// Probe for the native build prerequisites of tmux (libevent + ncurses dev
/// headers) via `pkg-config`. Matches what tmux's own `configure` uses, so a
/// positive probe here is a strong signal that `./scripts/build-vendored-tmux.sh`
/// will succeed. Absent pkg-config (or a missing `.pc` for libevent/ncurses)
/// returns `false` — the caller then skips the auto-build with a warning.
fn has_tmux_build_deps() -> bool {
    if !pkg_config_available() {
        return false;
    }

    if !pkg_config_has("libevent") {
        return false;
    }

    // ncurses ships under different .pc names depending on distro: `ncurses`
    // and `ncursesw` on Debian/Ubuntu, sometimes `tinfo` alone on minimal
    // images. Accept any match.
    pkg_config_has("ncurses") || pkg_config_has("ncursesw") || pkg_config_has("tinfo")
}

/// Probe for the native build prerequisites of ngIRCd. We disable every
/// optional feature in the build script (SSL, PAM, ident, tcp-wrappers,
/// iconv, ipv6), so the only hard requirements are a working C compiler
/// and `make`. Absent either, we skip the auto-build with a warning.
fn has_ircd_build_deps() -> bool {
    cc_available() && make_available()
}

fn pkg_config_available() -> bool {
    Command::new("pkg-config")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn pkg_config_has(pkg: &str) -> bool {
    Command::new("pkg-config")
        .args(["--exists", pkg])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn cc_available() -> bool {
    // `cc` is a POSIX-mandated wrapper that resolves to the system C
    // compiler (clang on macOS, gcc on most Linux distros). Probing it
    // instead of a specific vendor avoids false negatives on minimal
    // clang-only images.
    Command::new("cc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn make_available() -> bool {
    Command::new("make")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Invoke `scripts/build-vendored-tmux.sh` to produce `vendor/tmux/bin/tmux`.
///
/// Any failure here is a fatal build error when we reach this point: we've
/// already confirmed the prerequisites are present via `has_tmux_build_deps`,
/// so a script failure is something unexpected (network, disk, script bug)
/// that the developer needs to see, not silently skip.
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
        "cargo:warning=building vendored tmux {version} via {} (first build or version change; subsequent builds skip this step)",
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

/// Invoke `scripts/build-vendored-ircd.sh` to produce `vendor/ircd/bin/ngircd`.
/// Mirrors `build_vendored_tmux`: fatal on failure once we've cleared the
/// dep probe, since anything going wrong here is not something to silently
/// paper over.
fn build_vendored_ircd(manifest_dir: &Path, dev_path: &Path, version: &str) {
    let script = manifest_dir.join("scripts/build-vendored-ircd.sh");
    if !script.exists() {
        panic!(
            "vendored ngIRCd build script missing at {}; expected it to produce {}",
            script.display(),
            dev_path.display()
        );
    }

    println!(
        "cargo:warning=building vendored ngIRCd {version} via {} (first build or version change; subsequent builds skip this step)",
        script.display()
    );

    let status = Command::new("bash")
        .arg(&script)
        .current_dir(manifest_dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke {}: {e}", script.display()));

    if !status.success() {
        panic!(
            "{} exited with {status}; install the prerequisites (macOS: `xcode-select --install`; \
             Linux: `apt-get install build-essential`) and re-run, or set \
             RYVE_SKIP_VENDORED_IRCD_BUILD=1 to skip auto-build and stage the binary manually",
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
