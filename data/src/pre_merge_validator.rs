// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pre-merge validator for the Ryve branching model (spark ryve-c86e8cab).
//!
//! The branching model invariants, set by epic ryve-b8802f3b, are:
//!
//!   1. No user branch may ever be merged directly into `main`. The only
//!      legal target for a user branch is its owning Epic's `epic/<id>`
//!      branch.
//!   2. No actor may write to a branch owned by another actor. A user
//!      branch named `<actor>/<short>` is owned by `<actor>`; any other
//!      actor attempting to merge from — or push to — it is refused.
//!   3. `epic/<id>` → `main` is the only fast path from staged work to the
//!      trunk, and it is driven by the Merge Hand (epic G).
//!   4. Release-aware: `release/<version>` → `main` is accepted so the
//!      Release Manager can land a prepared release, and `epic/<id>` →
//!      `release/<version>` is accepted for epics that belong to a Release.
//!
//! This module is pure — it classifies branch names and returns typed
//! errors. It does not touch git or the workgraph database. Callers (the
//! Merge Hand, a future pre-merge git hook, CI gates) feed it the two
//! branch names (plus the pushing actor where applicable) and consume the
//! `Result<(), PreMergeError>`.

use thiserror::Error;

/// Canonical prefix for Epic branches (`epic/<spark_id>`).
pub const EPIC_PREFIX: &str = "epic/";
/// Canonical prefix for Release branches (`release/<version>`).
pub const RELEASE_PREFIX: &str = "release/";
/// Canonical prefix for Crew branches (`crew/<crew_id>`).
pub const CREW_PREFIX: &str = "crew/";
/// Canonical prefix for Merge branches (`merge/<id>`) used by the Merge Hand.
pub const MERGE_PREFIX: &str = "merge/";
/// The trunk branch name.
pub const MAIN_BRANCH: &str = "main";

/// Reserved prefixes that mark "system" branches — branches owned by the
/// workgraph itself, not by any single actor. A user-branch classification
/// must never match one of these, so `epic/alpha` cannot be mistaken for
/// actor `epic` working on `alpha`.
const RESERVED_PREFIXES: &[&str] = &[EPIC_PREFIX, RELEASE_PREFIX, CREW_PREFIX, MERGE_PREFIX];

/// Classification of a git branch name into its role in the branching
/// model. Computed from string shape only — this module never hits the
/// workgraph, so callers that need to confirm a classified branch still
/// exists or belongs to a particular spark must do that separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchKind {
    /// The trunk. Exactly `"main"`.
    Main,
    /// `epic/<spark_id>` — staging branch for an Epic.
    Epic { spark_id: String },
    /// `release/<version>` — Release Manager's staging branch.
    Release { version: String },
    /// `crew/<crew_id>` — the Merger's integration branch for a Crew.
    Crew { crew_id: String },
    /// `merge/<id>` — a Merge Hand's in-flight integration branch.
    Merge { id: String },
    /// `<actor>/<short>` — an actor-owned work branch.
    User { actor: String, short: String },
    /// Anything else. Callers decide whether an unknown branch is an error
    /// or a skip; this module treats it as "not classifiable" and rejects
    /// merges involving it.
    Unknown(String),
}

impl BranchKind {
    /// Classify a branch name.
    ///
    /// The order of checks matters: reserved prefixes are tested before
    /// the generic `<actor>/<short>` fallback so that e.g. `epic/foo`
    /// never resolves to actor `epic`, short `foo`.
    pub fn classify(name: &str) -> Self {
        if name == MAIN_BRANCH {
            return BranchKind::Main;
        }
        if let Some(rest) = name.strip_prefix(EPIC_PREFIX)
            && !rest.is_empty()
        {
            return BranchKind::Epic {
                spark_id: rest.to_string(),
            };
        }
        if let Some(rest) = name.strip_prefix(RELEASE_PREFIX)
            && !rest.is_empty()
        {
            return BranchKind::Release {
                version: rest.to_string(),
            };
        }
        if let Some(rest) = name.strip_prefix(CREW_PREFIX)
            && !rest.is_empty()
        {
            return BranchKind::Crew {
                crew_id: rest.to_string(),
            };
        }
        if let Some(rest) = name.strip_prefix(MERGE_PREFIX)
            && !rest.is_empty()
        {
            return BranchKind::Merge {
                id: rest.to_string(),
            };
        }

        // `<actor>/<short>` fallback. Reject names that start with a
        // reserved prefix (already handled above), names with multiple
        // slashes, empty halves, or whitespace in the actor component.
        if let Some((actor, short)) = name.split_once('/')
            && !actor.is_empty()
            && !short.is_empty()
            && !short.contains('/')
            && !actor.chars().any(char::is_whitespace)
            && !RESERVED_PREFIXES.iter().any(|p| name.starts_with(p))
        {
            return BranchKind::User {
                actor: actor.to_string(),
                short: short.to_string(),
            };
        }

        BranchKind::Unknown(name.to_string())
    }

    /// Return the owning actor of a user branch, or `None` for system
    /// branches.
    pub fn actor(&self) -> Option<&str> {
        match self {
            BranchKind::User { actor, .. } => Some(actor.as_str()),
            _ => None,
        }
    }
}

/// Errors produced when a proposed merge or write violates the branching
/// model.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PreMergeError {
    /// Case (1) from the epic invariants. A user branch may not land
    /// directly on `main`; the only legal staging target is its owning
    /// Epic branch.
    #[error(
        "refusing to merge user branch `{branch}` directly into main: \
         the only legal target for a user branch is its owning Epic's `epic/<id>` branch"
    )]
    UserBranchToMain { branch: String },

    /// Case (2) from the epic invariants. An actor may not write to a
    /// branch owned by a different actor.
    #[error(
        "actor `{actor}` may not write to branch `{branch}` owned by actor `{owner}`: \
         cross-actor mutation is refused"
    )]
    CrossActorWrite {
        actor: String,
        branch: String,
        owner: String,
    },

    /// Catch-all: the combination is not on the allow-list.
    #[error(
        "illegal merge `{from}` -> `{to}`: \
         no legal path in the branching model (source kind: {source_kind}, target kind: {target_kind})"
    )]
    IllegalTarget {
        from: String,
        to: String,
        source_kind: &'static str,
        target_kind: &'static str,
    },
}

/// Validate that a merge of `source` into `target` is legal under the
/// branching model.
///
/// Legal combinations:
/// - `<actor>/<short>` -> `epic/<id>`    (user -> epic, case 3)
/// - `epic/<id>`       -> `main`         (epic -> main via MergeHand, case 4)
/// - `epic/<id>`       -> `release/<v>`  (Release-aware variant of case 4)
/// - `release/<v>`     -> `main`         (Release Manager landing a release)
///
/// Everything else is rejected. In particular, `<actor>/<short>` -> `main`
/// is rejected with [`PreMergeError::UserBranchToMain`] (case 1).
pub fn validate_merge(source: &str, target: &str) -> Result<(), PreMergeError> {
    let s = BranchKind::classify(source);
    let t = BranchKind::classify(target);
    match (&s, &t) {
        (BranchKind::User { .. }, BranchKind::Main) => Err(PreMergeError::UserBranchToMain {
            branch: source.to_string(),
        }),
        (BranchKind::User { .. }, BranchKind::Epic { .. }) => Ok(()),
        (BranchKind::Epic { .. }, BranchKind::Main) => Ok(()),
        (BranchKind::Epic { .. }, BranchKind::Release { .. }) => Ok(()),
        (BranchKind::Release { .. }, BranchKind::Main) => Ok(()),
        _ => Err(PreMergeError::IllegalTarget {
            from: source.to_string(),
            to: target.to_string(),
            source_kind: kind_name(&s),
            target_kind: kind_name(&t),
        }),
    }
}

/// Validate that `actor` is permitted to write to `target_branch`.
///
/// User branches (`<actor>/<short>`) are owned by their actor prefix; any
/// other actor writing to one is refused with [`PreMergeError::CrossActorWrite`].
///
/// System branches (main / epic / release / crew / merge) are not owned by
/// a single actor and are gated by [`validate_merge`] instead; this
/// function returns `Ok(())` for them.
pub fn validate_actor_write(actor: &str, target_branch: &str) -> Result<(), PreMergeError> {
    if let BranchKind::User {
        actor: owner,
        short: _,
    } = BranchKind::classify(target_branch)
        && owner != actor
    {
        return Err(PreMergeError::CrossActorWrite {
            actor: actor.to_string(),
            branch: target_branch.to_string(),
            owner,
        });
    }
    Ok(())
}

/// Full pre-merge check: a pushing `actor` wants to merge `source` into
/// `target`. Applies both the cross-actor rule (the source must belong to
/// the pushing actor, if it is a user branch) and the merge-legality rule.
///
/// For system source branches (epic / release / crew / merge) the actor
/// check is a no-op — those merges are gated by their own machinery
/// (MergeHand, Release Manager) rather than by branch ownership.
pub fn validate_premerge(actor: &str, source: &str, target: &str) -> Result<(), PreMergeError> {
    if let BranchKind::User {
        actor: owner,
        short: _,
    } = BranchKind::classify(source)
        && owner != actor
    {
        return Err(PreMergeError::CrossActorWrite {
            actor: actor.to_string(),
            branch: source.to_string(),
            owner,
        });
    }
    validate_merge(source, target)
}

fn kind_name(k: &BranchKind) -> &'static str {
    match k {
        BranchKind::Main => "main",
        BranchKind::Epic { .. } => "epic",
        BranchKind::Release { .. } => "release",
        BranchKind::Crew { .. } => "crew",
        BranchKind::Merge { .. } => "merge",
        BranchKind::User { .. } => "user",
        BranchKind::Unknown(_) => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_main_and_system_branches() {
        assert_eq!(BranchKind::classify("main"), BranchKind::Main);
        assert_eq!(
            BranchKind::classify("epic/ryve-abc12345"),
            BranchKind::Epic {
                spark_id: "ryve-abc12345".into()
            }
        );
        assert_eq!(
            BranchKind::classify("release/0.1.0"),
            BranchKind::Release {
                version: "0.1.0".into()
            }
        );
        assert_eq!(
            BranchKind::classify("crew/cr-abcdef"),
            BranchKind::Crew {
                crew_id: "cr-abcdef".into()
            }
        );
        assert_eq!(
            BranchKind::classify("merge/xyz"),
            BranchKind::Merge { id: "xyz".into() }
        );
    }

    #[test]
    fn classify_user_branch_picks_actor_prefix() {
        assert_eq!(
            BranchKind::classify("alice/abc12345"),
            BranchKind::User {
                actor: "alice".into(),
                short: "abc12345".into()
            }
        );
        assert_eq!(
            BranchKind::classify("alice/abc12345").actor(),
            Some("alice")
        );
    }

    #[test]
    fn reserved_prefixes_never_resolve_to_user_branches() {
        // Must not be classified as User { actor: "epic", short: "foo" }.
        assert!(matches!(
            BranchKind::classify("epic/foo"),
            BranchKind::Epic { .. }
        ));
        assert!(matches!(
            BranchKind::classify("release/1.0.0"),
            BranchKind::Release { .. }
        ));
        assert!(matches!(
            BranchKind::classify("crew/cr-1"),
            BranchKind::Crew { .. }
        ));
        assert!(matches!(
            BranchKind::classify("merge/x"),
            BranchKind::Merge { .. }
        ));
    }

    #[test]
    fn unclassifiable_names_are_unknown() {
        assert!(matches!(BranchKind::classify(""), BranchKind::Unknown(_)));
        assert!(matches!(
            BranchKind::classify("just-a-name"),
            BranchKind::Unknown(_)
        ));
        assert!(matches!(
            BranchKind::classify("a/b/c"),
            BranchKind::Unknown(_)
        ));
        assert!(matches!(
            BranchKind::classify("/short"),
            BranchKind::Unknown(_)
        ));
        assert!(matches!(
            BranchKind::classify("actor/"),
            BranchKind::Unknown(_)
        ));
    }

    #[test]
    fn case_1_user_branch_to_main_is_rejected() {
        let err = validate_merge("alice/abc12345", "main").unwrap_err();
        assert!(matches!(err, PreMergeError::UserBranchToMain { .. }));
    }

    #[test]
    fn case_2_cross_actor_write_is_rejected() {
        let err = validate_actor_write("bob", "alice/abc12345").unwrap_err();
        match err {
            PreMergeError::CrossActorWrite {
                actor,
                branch,
                owner,
            } => {
                assert_eq!(actor, "bob");
                assert_eq!(owner, "alice");
                assert_eq!(branch, "alice/abc12345");
            }
            other => panic!("expected CrossActorWrite, got {other:?}"),
        }
    }

    #[test]
    fn case_3_user_to_epic_is_accepted() {
        validate_merge("alice/abc12345", "epic/ryve-b8802f3b").unwrap();
        validate_premerge("alice", "alice/abc12345", "epic/ryve-b8802f3b").unwrap();
    }

    #[test]
    fn case_4_epic_to_main_is_accepted() {
        validate_merge("epic/ryve-b8802f3b", "main").unwrap();
    }

    #[test]
    fn same_actor_write_is_allowed() {
        validate_actor_write("alice", "alice/abc12345").unwrap();
    }

    #[test]
    fn writes_to_system_branches_bypass_actor_check() {
        // No single actor owns main/epic/release/crew/merge — those
        // branches are gated by validate_merge, not validate_actor_write.
        validate_actor_write("alice", "main").unwrap();
        validate_actor_write("alice", "epic/ryve-abc12345").unwrap();
        validate_actor_write("alice", "release/0.1.0").unwrap();
        validate_actor_write("alice", "crew/cr-xyz").unwrap();
    }

    #[test]
    fn validate_premerge_rejects_source_owned_by_other_actor() {
        // bob tries to push a merge whose source is alice/... — refused.
        let err = validate_premerge("bob", "alice/abc12345", "epic/ryve-b8802f3b").unwrap_err();
        assert!(matches!(err, PreMergeError::CrossActorWrite { .. }));
    }

    #[test]
    fn epic_to_release_is_accepted() {
        validate_merge("epic/ryve-b8802f3b", "release/0.1.0").unwrap();
    }

    #[test]
    fn release_to_main_is_accepted() {
        validate_merge("release/0.1.0", "main").unwrap();
    }

    #[test]
    fn user_to_user_is_rejected_as_illegal_target() {
        let err = validate_merge("alice/abc12345", "bob/def67890").unwrap_err();
        assert!(matches!(err, PreMergeError::IllegalTarget { .. }));
    }

    #[test]
    fn epic_to_epic_is_rejected_as_illegal_target() {
        let err = validate_merge("epic/ryve-one", "epic/ryve-two").unwrap_err();
        assert!(matches!(err, PreMergeError::IllegalTarget { .. }));
    }

    #[test]
    fn unknown_source_or_target_is_rejected() {
        assert!(validate_merge("some-detached-ref", "main").is_err());
        assert!(validate_merge("alice/abc12345", "").is_err());
    }
}
