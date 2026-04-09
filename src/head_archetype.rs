// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Head archetype model + registry [sp-fbf2a519 / ryve-982bddb8].
//!
//! A **Head** is a coding-agent subprocess that orchestrates a Crew of
//! Hands (see `docs/HEAD_ARCHETYPES.md`). Every Head is spawned under an
//! *archetype* — a typed contract that spells out what the Head is for,
//! which prompt template it boots with, which coding agent to launch by
//! default, which Hand archetypes it is allowed to delegate to, and what
//! write discipline its Crew operates under.
//!
//! Archetypes are declared once, here, in a data-driven registry. The CLI
//! (`ryve head archetype list`) and, eventually, `ryve head spawn
//! --archetype <name>` look archetypes up by name — neither the CLI parser
//! nor the spawn path hardcodes archetype identifiers. Adding a new
//! archetype is a matter of extending [`Registry::builtins`] (or dropping
//! a future TOML overlay into `.ryve/`); no CLI-parser edit is required.
//!
//! ## Invariants
//!
//! - Archetype names are unique within a workshop. [`Registry::new`]
//!   returns [`RegistryError::DuplicateName`] if two archetypes share a
//!   name, and [`Registry::builtins`] is covered by a unit test to make
//!   sure the compiled-in defaults never regress on this.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How much write authority a Head's Crew has. Heads themselves never
/// edit source; this discipline describes what their delegated Hands are
/// allowed to do on disk.
///
/// The ladder is deliberately coarse — spark work falls cleanly into one
/// of these three buckets today. Finer-grained per-capability rules can
/// be layered on top later without breaking the archetype model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteDiscipline {
    /// Read-only: Hands may inspect the repo and post comments / create
    /// sparks, but must not edit files, create branches, or run
    /// destructive commands. Used by Research and Review archetypes.
    ReadOnly,
    /// Hands edit code inside their own worktree branches. Integration
    /// into `main` is not permitted — that is the Merger's job.
    WorktreeWrite,
    /// Integration authority: the Crew's Merger may land worktree
    /// branches on an integration branch and open a PR. No Hand is
    /// allowed to push directly to `main`.
    IntegrationOnly,
}

/// A typed description of a Head archetype. See `docs/HEAD_ARCHETYPES.md`
/// for the narrative contract; this struct is the machine-readable
/// counterpart so the CLI and spawn path can look one up by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadArchetype {
    /// Unique identifier within a workshop, e.g. `"build"`. Used as the
    /// lookup key for `ryve head spawn --archetype <name>`.
    pub name: String,

    /// One-line human description, rendered by `ryve head archetype list`.
    pub description: String,

    /// Path to the prompt template that boots a Head of this archetype.
    /// Relative paths are resolved against the workshop `.ryve/` dir.
    /// The file itself does not need to exist at registry-construction
    /// time; the spawn path is responsible for validating it at use.
    pub prompt_template_path: PathBuf,

    /// Default coding agent command (e.g. `"claude"`, `"codex"`) used
    /// when the user does not override with `--agent`.
    pub default_agent: String,

    /// Names of Hand archetypes this Head is allowed to spawn. Empty
    /// means "none" — a Head that cannot delegate (rare but legal).
    pub allowed_hand_archetypes: Vec<String>,

    /// Write authority of the Crew this Head orchestrates.
    pub write_discipline: WriteDiscipline,
}

/// Errors that can happen when constructing a [`Registry`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    /// Two archetypes in the input shared the same `name`.
    ///
    /// This is the only way the "archetype names are unique within a
    /// workshop" invariant can be violated at construction time;
    /// rejecting it here keeps downstream lookups (`get_by_name`) a
    /// simple linear scan with no disambiguation logic.
    #[error("duplicate archetype name: {0}")]
    DuplicateName(String),
}

/// In-memory registry of Head archetypes. Cheap to construct (the
/// compiled-in default set is small and fixed), so the CLI builds a
/// fresh one per invocation rather than caching globally.
#[derive(Debug, Clone)]
pub struct Registry {
    archetypes: Vec<HeadArchetype>,
}

impl Registry {
    /// Build a registry from an explicit list of archetypes. Returns
    /// [`RegistryError::DuplicateName`] if any two entries share a
    /// `name`. This is the single choke point that enforces the
    /// uniqueness invariant — both [`Registry::builtins`] and any future
    /// TOML loader must go through it.
    pub fn new(archetypes: Vec<HeadArchetype>) -> Result<Self, RegistryError> {
        for (i, a) in archetypes.iter().enumerate() {
            if archetypes[..i].iter().any(|b| b.name == a.name) {
                return Err(RegistryError::DuplicateName(a.name.clone()));
            }
        }
        Ok(Self { archetypes })
    }

    /// Registry populated with Ryve's built-in archetypes: `build`,
    /// `research`, and `review`. This set mirrors the three standard
    /// archetypes documented in `docs/HEAD_ARCHETYPES.md`.
    ///
    /// Panics only if the compiled-in list is self-inconsistent
    /// (duplicate name) — a unit test asserts this can never happen in
    /// practice.
    pub fn builtins() -> Self {
        Self::new(vec![
            HeadArchetype {
                name: "build".to_string(),
                description: "Ship code that satisfies acceptance criteria via a Crew + Merger."
                    .to_string(),
                prompt_template_path: PathBuf::from("head_prompts/build.md"),
                default_agent: "claude".to_string(),
                allowed_hand_archetypes: vec![
                    "implementer".to_string(),
                    "refactor".to_string(),
                    "test".to_string(),
                    "merger".to_string(),
                ],
                write_discipline: WriteDiscipline::IntegrationOnly,
            },
            HeadArchetype {
                name: "research".to_string(),
                description: "Reduce uncertainty with read-only investigation Hands; \
                              output is a recommendation comment, never code."
                    .to_string(),
                prompt_template_path: PathBuf::from("head_prompts/research.md"),
                default_agent: "claude".to_string(),
                allowed_hand_archetypes: vec!["investigator".to_string()],
                write_discipline: WriteDiscipline::ReadOnly,
            },
            HeadArchetype {
                name: "review".to_string(),
                description: "Critique existing code / designs / PRs with read-only reviewer \
                              Hands; output is a structured review comment."
                    .to_string(),
                prompt_template_path: PathBuf::from("head_prompts/review.md"),
                default_agent: "claude".to_string(),
                allowed_hand_archetypes: vec!["reviewer".to_string()],
                write_discipline: WriteDiscipline::ReadOnly,
            },
        ])
        .expect("builtin archetypes must have unique names")
    }

    /// All archetypes in registration order. Intended for listing
    /// commands and UI; stable across calls within a single registry.
    pub fn all(&self) -> &[HeadArchetype] {
        &self.archetypes
    }

    /// Look up an archetype by exact name, case-sensitive. Returns
    /// `None` if no archetype matches — callers should surface the
    /// list of known names in their error message.
    ///
    /// Reserved for the `ryve head spawn --archetype <name>` path
    /// tracked by the follow-up sparks that this one unblocks
    /// (ryve-53bb0bac / ryve-e4cadc03); exercised today only by unit
    /// tests, hence the `allow(dead_code)`.
    #[allow(dead_code)]
    pub fn get_by_name(&self, name: &str) -> Option<&HeadArchetype> {
        self.archetypes.iter().find(|a| a.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_contain_the_three_standard_archetypes() {
        let reg = Registry::builtins();
        let names: Vec<&str> = reg.all().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["build", "research", "review"]);
    }

    /// Invariant: archetype names must be unique within a workshop. The
    /// compiled-in default set is the most likely place to accidentally
    /// regress on this (adding a duplicate during a copy-paste edit), so
    /// guard the builder directly.
    #[test]
    fn builtins_have_unique_names() {
        // `Registry::builtins` panics on duplicate, but build an
        // explicit HashSet too so the failure mode is obvious.
        let reg = Registry::builtins();
        let mut seen = std::collections::HashSet::new();
        for a in reg.all() {
            assert!(
                seen.insert(a.name.as_str()),
                "duplicate builtin archetype name: {}",
                a.name
            );
        }
    }

    #[test]
    fn new_rejects_duplicate_names() {
        let a = HeadArchetype {
            name: "dup".to_string(),
            description: String::new(),
            prompt_template_path: PathBuf::new(),
            default_agent: "claude".to_string(),
            allowed_hand_archetypes: Vec::new(),
            write_discipline: WriteDiscipline::ReadOnly,
        };
        let err = Registry::new(vec![a.clone(), a]).unwrap_err();
        assert_eq!(err, RegistryError::DuplicateName("dup".to_string()));
    }

    #[test]
    fn get_by_name_finds_registered_and_rejects_unknown() {
        let reg = Registry::builtins();
        assert_eq!(reg.get_by_name("build").map(|a| a.name.as_str()), Some("build"));
        assert!(reg.get_by_name("nonexistent").is_none());
        // Case-sensitive on purpose: "Build" is not the same as "build".
        assert!(reg.get_by_name("Build").is_none());
    }

    #[test]
    fn build_archetype_allows_merger_and_has_integration_discipline() {
        let reg = Registry::builtins();
        let build = reg.get_by_name("build").expect("build archetype");
        assert_eq!(build.write_discipline, WriteDiscipline::IntegrationOnly);
        assert!(
            build.allowed_hand_archetypes.iter().any(|h| h == "merger"),
            "build head must be allowed to spawn a merger"
        );
    }

    #[test]
    fn research_and_review_are_read_only() {
        let reg = Registry::builtins();
        for name in ["research", "review"] {
            let a = reg.get_by_name(name).expect(name);
            assert_eq!(
                a.write_discipline,
                WriteDiscipline::ReadOnly,
                "{name} must be read-only"
            );
            assert!(
                !a.allowed_hand_archetypes.iter().any(|h| h == "merger"),
                "{name} must not be allowed to spawn a merger"
            );
        }
    }

    /// Round-trip the default set through serde JSON. This locks in
    /// the on-disk schema so a future TOML/JSON overlay under `.ryve/`
    /// can be added without surprise migrations.
    #[test]
    fn archetype_serializes_round_trip() {
        let reg = Registry::builtins();
        for a in reg.all() {
            let json = serde_json::to_string(a).expect("serialize");
            let back: HeadArchetype = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(&back, a);
        }
    }
}
