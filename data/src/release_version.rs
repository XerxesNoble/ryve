// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Strict semver helper for Release versions.
//!
//! Versions are strict `MAJOR.MINOR.PATCH` — no pre-release tags, no build
//! metadata. This module is the single source of truth for release version
//! math: parsing, formatting, and deterministic bumping from the previous
//! closed release.
//!
//! The first-ever release bumps from the implicit baseline `0.0.0`:
//! * `next(None, Bump::Major) == 1.0.0`
//! * `next(None, Bump::Minor) == 0.1.0`
//! * `next(None, Bump::Patch) == 0.0.1`

use std::fmt;

/// A strict semver triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub const ZERO: Version = Version {
        major: 0,
        minor: 0,
        patch: 0,
    };

    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The kind of version bump requested when creating a new release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bump {
    Major,
    Minor,
    Patch,
}

/// Errors produced by the semver helper.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VersionError {
    #[error("invalid semver `{input}`: {reason}")]
    Invalid { input: String, reason: &'static str },

    #[error("downgrade rejected: next version {next} would not advance past {prev}")]
    Downgrade { prev: Version, next: Version },
}

/// Parse a strict `MAJOR.MINOR.PATCH` string.
///
/// Rejects pre-release suffixes, build metadata, leading `v`, whitespace,
/// leading zeros on multi-digit components, and any non-digit component.
pub fn parse(input: &str) -> Result<Version, VersionError> {
    fn invalid(input: &str, reason: &'static str) -> VersionError {
        VersionError::Invalid {
            input: input.to_string(),
            reason,
        }
    }

    if input.is_empty() {
        return Err(invalid(input, "empty string"));
    }
    if input.trim() != input {
        return Err(invalid(input, "surrounding whitespace"));
    }
    if input.contains('-') {
        return Err(invalid(input, "pre-release tags not allowed"));
    }
    if input.contains('+') {
        return Err(invalid(input, "build metadata not allowed"));
    }

    let parts: Vec<&str> = input.split('.').collect();
    if parts.len() != 3 {
        return Err(invalid(input, "must have exactly three components"));
    }

    let mut out = [0u64; 3];
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            return Err(invalid(input, "empty component"));
        }
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid(input, "component is not a decimal integer"));
        }
        if part.len() > 1 && part.starts_with('0') {
            return Err(invalid(input, "leading zero in component"));
        }
        out[i] = part
            .parse::<u64>()
            .map_err(|_| invalid(input, "component overflows u64"))?;
    }

    Ok(Version {
        major: out[0],
        minor: out[1],
        patch: out[2],
    })
}

/// Format a `Version` as its canonical `MAJOR.MINOR.PATCH` string.
pub fn format(v: Version) -> String {
    v.to_string()
}

/// Compute the next release version.
///
/// `prev` is the most recent closed release's version, or `None` if there
/// has never been a closed release. The first-ever release bumps from
/// `0.0.0` using `bump`:
/// * `next(None, Major) == 1.0.0`
/// * `next(None, Minor) == 0.1.0`
/// * `next(None, Patch) == 0.0.1`
///
/// For subsequent releases the bump kind strictly advances `prev`. The
/// function rejects any computation that would not strictly advance the
/// version (a safety net — with a valid `prev` and `Bump` a bump always
/// advances, but this guards against future changes and misuse).
pub fn next(prev: Option<Version>, bump: Bump) -> Result<Version, VersionError> {
    let base = prev.unwrap_or(Version::ZERO);
    let candidate = match bump {
        Bump::Major => Version {
            major: base.major.saturating_add(1),
            minor: 0,
            patch: 0,
        },
        Bump::Minor => Version {
            major: base.major,
            minor: base.minor.saturating_add(1),
            patch: 0,
        },
        Bump::Patch => Version {
            major: base.major,
            minor: base.minor,
            patch: base.patch.saturating_add(1),
        },
    };

    if candidate <= base {
        return Err(VersionError::Downgrade {
            prev: base,
            next: candidate,
        });
    }

    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_triples() {
        assert_eq!(parse("0.0.0").unwrap(), Version::new(0, 0, 0));
        assert_eq!(parse("1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(parse("10.20.30").unwrap(), Version::new(10, 20, 30));
    }

    #[test]
    fn format_roundtrips() {
        let v = Version::new(4, 5, 6);
        assert_eq!(format(v), "4.5.6");
        assert_eq!(parse(&format(v)).unwrap(), v);
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in [
            "",
            " 1.2.3",
            "1.2.3 ",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "1.2.3-alpha",
            "1.2.3+build",
            "1.2.3-rc.1",
            "1..3",
            ".1.2",
            "1.2.",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "a.b.c",
            "1.2.x",
            "-1.2.3",
        ] {
            assert!(
                matches!(parse(bad), Err(VersionError::Invalid { .. })),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn first_release_from_none() {
        assert_eq!(next(None, Bump::Major).unwrap(), Version::new(1, 0, 0));
        assert_eq!(next(None, Bump::Minor).unwrap(), Version::new(0, 1, 0));
        assert_eq!(next(None, Bump::Patch).unwrap(), Version::new(0, 0, 1));
    }

    #[test]
    fn major_bump_resets_minor_and_patch() {
        let prev = Version::new(1, 4, 7);
        assert_eq!(next(Some(prev), Bump::Major).unwrap(), Version::new(2, 0, 0));
    }

    #[test]
    fn minor_bump_resets_patch() {
        let prev = Version::new(1, 4, 7);
        assert_eq!(next(Some(prev), Bump::Minor).unwrap(), Version::new(1, 5, 0));
    }

    #[test]
    fn patch_bump_increments_patch_only() {
        let prev = Version::new(1, 4, 7);
        assert_eq!(next(Some(prev), Bump::Patch).unwrap(), Version::new(1, 4, 8));
    }

    #[test]
    fn bumps_strictly_advance() {
        let prev = Version::new(0, 0, 1);
        assert!(next(Some(prev), Bump::Patch).unwrap() > prev);
        assert!(next(Some(prev), Bump::Minor).unwrap() > prev);
        assert!(next(Some(prev), Bump::Major).unwrap() > prev);
    }

    #[test]
    fn downgrade_rejected_on_saturating_overflow() {
        // At u64::MAX the saturating add produces the same value, which
        // would be a non-advancing "bump" — it must be rejected.
        let prev = Version::new(u64::MAX, 0, 0);
        assert!(matches!(
            next(Some(prev), Bump::Major),
            Err(VersionError::Downgrade { .. })
        ));

        let prev = Version::new(0, u64::MAX, 0);
        assert!(matches!(
            next(Some(prev), Bump::Minor),
            Err(VersionError::Downgrade { .. })
        ));

        let prev = Version::new(0, 0, u64::MAX);
        assert!(matches!(
            next(Some(prev), Bump::Patch),
            Err(VersionError::Downgrade { .. })
        ));
    }

    #[test]
    fn ordering_is_lexicographic_by_component() {
        assert!(Version::new(1, 0, 0) > Version::new(0, 9, 9));
        assert!(Version::new(1, 2, 0) > Version::new(1, 1, 9));
        assert!(Version::new(1, 2, 3) > Version::new(1, 2, 2));
    }
}
