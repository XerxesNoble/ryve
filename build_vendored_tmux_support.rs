// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pure helpers shared between `build.rs` and integration tests for the
//! vendored tmux build. Kept in a plain file (included via `#[path]`) so
//! build scripts and tests can exercise the same logic without pulling in
//! a separate crate. See also `scripts/build-vendored-tmux.sh`, which writes
//! the stamp file read back here.

use std::path::{Path, PathBuf};
use std::{fs, io};

/// Canonical location for the "which version did we build last?" stamp.
/// Co-located with the binary so a `rm -rf vendor/tmux/bin` resets both.
pub fn stamp_path(bin_dir: &Path) -> PathBuf {
    bin_dir.join(".version")
}

/// Returns the trimmed stamp contents, or `None` if the file is absent or
/// unreadable. Any I/O failure collapses to `None` — the caller treats that
/// the same as "rebuild needed".
pub fn read_stamp(stamp_path: &Path) -> Option<String> {
    fs::read_to_string(stamp_path)
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Writes `version` (with a trailing newline so `cat` is well-behaved) to
/// `stamp_path`, creating parent directories as needed.
pub fn write_stamp(stamp_path: &Path, version: &str) -> io::Result<()> {
    if let Some(parent) = stamp_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(stamp_path, format!("{}\n", version.trim()))
}

/// True iff the stamp exists and its trimmed contents equal `version`.
pub fn stamp_matches(stamp_path: &Path, version: &str) -> bool {
    read_stamp(stamp_path).as_deref() == Some(version.trim())
}
