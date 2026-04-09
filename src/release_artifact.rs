// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Release-artifact builder.
//!
//! Closing a Release produces a single built binary at a deterministic path so
//! the Release Manager can hand it off to Atlas without ambiguity:
//!
//! ```text
//! .ryve/releases/<version>/ryve-<version>-<target>
//! ```
//!
//! The public entry point is [`build_release_artifact`], which runs a
//! release-profile `cargo build` against the workshop's project root, verifies
//! the resulting binary, and copies it into place. Build failure is terminal:
//! if the compiler errors out, the artifact path is *not* touched — callers
//! that observe a [`BuildError`] can be certain no partial file was left
//! behind.
//!
//! The build runs entirely off the UI thread: `cargo` is launched via
//! `tokio::process::Command` and awaited on the tokio runtime, so the iced
//! event loop is never blocked.

use std::io;
use std::path::{Path, PathBuf};

use tokio::process::Command;

/// The default cargo binary name produced by the `ryve` workshop crate.
pub const DEFAULT_BINARY_NAME: &str = "ryve";

/// Relative path (from the workshop root) where release artifacts are
/// written. One subdirectory per version.
pub const RELEASES_DIR: &str = ".ryve/releases";

/// Errors that can surface while building a release artifact.
///
/// Every variant is terminal for the close operation — on any error, no file
/// is written to the deterministic artifact path.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The supplied version string did not look like a canonical semver
    /// `MAJOR.MINOR.PATCH`. We intentionally stay strict here because the
    /// version is baked into the artifact filename.
    #[error("invalid release version: {0}")]
    InvalidVersion(String),

    /// Failed to discover the host target triple via `rustc -vV`.
    #[error("unable to determine host target triple: {0}")]
    ToolchainQueryFailed(String),

    /// `cargo build --release` exited with a non-zero status.
    ///
    /// `stderr` is captured so the Release Manager can show a useful failure
    /// reason to the user.
    #[error("cargo build --release failed (exit {exit_code}): {stderr}")]
    CargoBuildFailed { exit_code: i32, stderr: String },

    /// The build reported success but the expected binary was not found on
    /// disk afterwards. This usually means the caller passed the wrong
    /// `binary_name` for the fixture.
    #[error("release build produced no binary at {0}")]
    BinaryMissing(PathBuf),

    /// The produced binary exists but is empty, which we treat as a failed
    /// build (cargo should never emit a zero-byte executable).
    #[error("release build produced an empty binary at {0}")]
    BinaryEmpty(PathBuf),

    /// Catch-all for filesystem I/O errors while staging the artifact.
    #[error("release artifact I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Build the workshop's `ryve` binary at release profile and stage it under
/// `.ryve/releases/<version>/ryve-<version>-<target>` inside `workshop_root`.
///
/// On success, returns the deterministic artifact path.
///
/// On failure, returns a typed [`BuildError`] and guarantees that no file was
/// written to the artifact path — the Release Manager can treat a failure as
/// "nothing to hand off".
pub async fn build_release_artifact(
    workshop_root: &Path,
    version: &str,
) -> Result<PathBuf, BuildError> {
    build_release_artifact_inner(workshop_root, version, DEFAULT_BINARY_NAME).await
}

/// Test-visible core of [`build_release_artifact`]. Lets integration tests
/// point at a fixture cargo project whose binary is not named `ryve`.
pub async fn build_release_artifact_inner(
    workshop_root: &Path,
    version: &str,
    binary_name: &str,
) -> Result<PathBuf, BuildError> {
    validate_version(version)?;

    let target_triple = host_target_triple().await?;

    // 1. Build. A non-zero cargo exit short-circuits before we ever touch
    //    the artifact directory, which is how the "no partial file left
    //    behind" invariant is enforced.
    let output = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(workshop_root)
        .output()
        .await
        .map_err(BuildError::Io)?;

    if !output.status.success() {
        return Err(BuildError::CargoBuildFailed {
            exit_code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    // 2. Verify the binary cargo just produced.
    let built_binary = workshop_root
        .join("target")
        .join("release")
        .join(platform_binary_filename(binary_name));

    let metadata = match tokio::fs::metadata(&built_binary).await {
        Ok(md) => md,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(BuildError::BinaryMissing(built_binary));
        }
        Err(e) => return Err(BuildError::Io(e)),
    };
    if metadata.len() == 0 {
        return Err(BuildError::BinaryEmpty(built_binary));
    }

    // 3. Stage into the deterministic artifact path.
    let artifact_path = artifact_path_for(workshop_root, version, &target_triple);
    if let Some(parent) = artifact_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Copy through a sibling temp file + atomic rename so an interrupted copy
    // can never leave a half-written artifact at the deterministic path. If
    // the rename fails we explicitly clear the temp file.
    let tmp_path = artifact_path.with_extension("partial");
    if let Err(e) = tokio::fs::copy(&built_binary, &tmp_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(BuildError::Io(e));
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, &artifact_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(BuildError::Io(e));
    }

    Ok(artifact_path)
}

/// Compute the deterministic artifact path for a given release.
///
/// Exposed so tests (and the Release Manager UI) can assert on the expected
/// location without running an actual build.
pub fn artifact_path_for(workshop_root: &Path, version: &str, target_triple: &str) -> PathBuf {
    workshop_root
        .join(RELEASES_DIR)
        .join(version)
        .join(format!("ryve-{version}-{target_triple}"))
}

/// Strict canonical-semver check: MAJOR.MINOR.PATCH, digits only.
///
/// We do *not* accept pre-release or build metadata here — release artifacts
/// are numbered with a plain x.y.z so the filename stays filesystem-friendly.
fn validate_version(version: &str) -> Result<(), BuildError> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return Err(BuildError::InvalidVersion(version.to_string()));
    }
    for part in parts {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return Err(BuildError::InvalidVersion(version.to_string()));
        }
    }
    Ok(())
}

/// Public wrapper around [`host_target_triple`] for use by the CLI close
/// flow, which needs the triple to compute the anticipated artifact path
/// before the build runs.
pub async fn host_target_triple_for_cli() -> Result<String, BuildError> {
    host_target_triple().await
}

/// Query `rustc -vV` for the host target triple. We shell out (rather than
/// hard-coding from `std::env::consts`) because the rustc triple is what
/// cargo actually uses under `target/release`, and the release naming
/// contract is "the triple the compiler targeted".
async fn host_target_triple() -> Result<String, BuildError> {
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .await
        .map_err(|e| BuildError::ToolchainQueryFailed(e.to_string()))?;
    if !output.status.success() {
        return Err(BuildError::ToolchainQueryFailed(format!(
            "rustc -vV exited with {}",
            output.status
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("host:") {
            let triple = rest.trim();
            if !triple.is_empty() {
                return Ok(triple.to_string());
            }
        }
    }
    Err(BuildError::ToolchainQueryFailed(
        "rustc -vV did not emit a 'host:' line".to_string(),
    ))
}

/// Append `.exe` on Windows; cargo's release output follows the host's
/// executable-suffix rules.
fn platform_binary_filename(binary_name: &str) -> String {
    if cfg!(windows) {
        format!("{binary_name}.exe")
    } else {
        binary_name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture_cargo_project(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.1\"\nedition = \"2021\"\n\n\
                 [[bin]]\nname = \"{name}\"\npath = \"src/main.rs\"\n\n\
                 [profile.release]\nopt-level = 0\nlto = false\ncodegen-units = 256\nstrip = false\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("src").join("main.rs"), body).unwrap();
    }

    #[test]
    fn validates_semver_shape() {
        assert!(validate_version("1.2.3").is_ok());
        assert!(validate_version("0.0.0").is_ok());
        assert!(matches!(
            validate_version("1.2"),
            Err(BuildError::InvalidVersion(_))
        ));
        assert!(matches!(
            validate_version("1.2.3-beta"),
            Err(BuildError::InvalidVersion(_))
        ));
        assert!(matches!(
            validate_version("v1.2.3"),
            Err(BuildError::InvalidVersion(_))
        ));
        assert!(matches!(
            validate_version(""),
            Err(BuildError::InvalidVersion(_))
        ));
    }

    #[test]
    fn artifact_path_is_deterministic() {
        let root = PathBuf::from("/tmp/workshop");
        let p = artifact_path_for(&root, "1.2.3", "aarch64-apple-darwin");
        assert_eq!(
            p,
            PathBuf::from("/tmp/workshop/.ryve/releases/1.2.3/ryve-1.2.3-aarch64-apple-darwin")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn builds_trivial_fixture_to_deterministic_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fixture_cargo_project(
            root,
            "ryve_release_fixture",
            "fn main() { println!(\"hi\"); }\n",
        );

        let version = "9.9.9";
        let produced = build_release_artifact_inner(root, version, "ryve_release_fixture")
            .await
            .expect("fixture build should succeed");

        let triple = host_target_triple().await.unwrap();
        let expected = artifact_path_for(root, version, &triple);
        assert_eq!(produced, expected);

        // Artifact must actually exist on disk and be non-empty.
        let md = tokio::fs::metadata(&produced).await.unwrap();
        assert!(md.is_file());
        assert!(md.len() > 0);

        // And the parent directory matches the deterministic layout.
        assert_eq!(
            produced.parent().unwrap(),
            root.join(".ryve/releases").join(version)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_failure_leaves_no_partial_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Deliberately broken fixture: main.rs will not compile.
        fixture_cargo_project(
            root,
            "ryve_release_broken",
            "fn main() { this_is_not_valid_rust(); }\n",
        );

        let version = "9.9.9";
        let err = build_release_artifact_inner(root, version, "ryve_release_broken")
            .await
            .expect_err("broken fixture must fail");
        match err {
            BuildError::CargoBuildFailed { .. } => {}
            other => panic!("expected CargoBuildFailed, got {other:?}"),
        }

        // The invariant we care about: no file at the artifact path, and
        // no stray `.partial` sidecar either.
        let triple = host_target_triple().await.unwrap();
        let expected = artifact_path_for(root, version, &triple);
        assert!(
            !expected.exists(),
            "artifact path must not exist after a failed build"
        );
        assert!(
            !expected.with_extension("partial").exists(),
            "partial sidecar must not linger after a failed build"
        );
    }
}
