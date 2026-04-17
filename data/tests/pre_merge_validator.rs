// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for the pre-merge validator (spark ryve-c86e8cab).
//!
//! Epic ryve-b8802f3b's acceptance row:
//!   > Unit + integration tests cover: user branch rejected at main,
//!   > cross-user mutation rejected, legal user→epic merge accepted,
//!   > legal epic→main merge accepted.
//!
//! These four cases are covered here as external integration tests that
//! exercise the public API of `data::pre_merge_validator` exactly as a
//! downstream consumer (a Merge Hand, a git hook, a CI gate) would.
//!
//! Every test drives the same flow the production hook would: classify
//! the two branches by name, hand them to `validate_merge` /
//! `validate_premerge`, and assert the outcome. No git repos or DB
//! fixtures are needed because the validator is pure; that is itself an
//! invariant worth protecting — making the validator depend on the
//! filesystem would regress spawn-time and hook-time callers that don't
//! have a checkout on hand.

use data::pre_merge_validator::{
    BranchKind, PreMergeError, validate_actor_write, validate_merge, validate_premerge,
};

// ───────────── The four required legality cases ─────────────

/// Case 1 of 4: user branch rejected at main.
#[test]
fn case_1_user_branch_rejected_at_main() {
    let err = validate_merge("alice/abc12345", "main").expect_err("must reject");
    match err {
        PreMergeError::UserBranchToMain { branch } => {
            assert_eq!(branch, "alice/abc12345");
        }
        other => panic!("expected UserBranchToMain, got {other:?}"),
    }

    // And the combined premerge check (actor=alice, pushing her own
    // branch) still rejects because the target is main.
    let err = validate_premerge("alice", "alice/abc12345", "main").expect_err("must reject");
    assert!(matches!(err, PreMergeError::UserBranchToMain { .. }));
}

/// Case 2 of 4: cross-user mutation rejected.
#[test]
fn case_2_cross_user_mutation_rejected() {
    // bob may not write to alice's branch.
    let err = validate_actor_write("bob", "alice/abc12345").expect_err("must reject");
    match err {
        PreMergeError::CrossActorWrite {
            actor,
            branch,
            owner,
        } => {
            assert_eq!(actor, "bob");
            assert_eq!(branch, "alice/abc12345");
            assert_eq!(owner, "alice");
        }
        other => panic!("expected CrossActorWrite, got {other:?}"),
    }

    // bob also may not merge from alice's branch into alice's Epic.
    let err =
        validate_premerge("bob", "alice/abc12345", "epic/ryve-b8802f3b").expect_err("must reject");
    assert!(matches!(err, PreMergeError::CrossActorWrite { .. }));
}

/// Case 3 of 4: legal user→epic merge accepted.
#[test]
fn case_3_user_to_epic_accepted() {
    validate_merge("alice/abc12345", "epic/ryve-b8802f3b").expect("must accept");
    validate_premerge("alice", "alice/abc12345", "epic/ryve-b8802f3b").expect("must accept");
}

/// Case 4 of 4: legal epic→main merge accepted.
#[test]
fn case_4_epic_to_main_accepted() {
    validate_merge("epic/ryve-b8802f3b", "main").expect("must accept");
}

// ───────────── Supporting coverage ─────────────

/// Cross-actor refusal must trigger *before* the merge-legality check so
/// that the error reported back to the caller is the specific identity
/// violation, not a generic "illegal target" — the Merge Hand relies on
/// this to surface the right message to the human reviewer.
#[test]
fn cross_actor_refusal_is_the_primary_error() {
    // bob pushes alice's branch. target would otherwise be legal
    // (user → epic is on the allow-list), so if the order of checks were
    // swapped this would succeed.
    let err =
        validate_premerge("bob", "alice/abc12345", "epic/ryve-b8802f3b").expect_err("must reject");
    match err {
        PreMergeError::CrossActorWrite { .. } => {}
        other => panic!("expected CrossActorWrite as primary error, got {other:?}"),
    }
}

/// A user branch rejected at main must stay rejected even when the
/// pushing actor is the branch's owner — the rule is about the branching
/// topology, not about identity. Without this assertion, an accidental
/// loosening of the validator ("alice can do anything with alice/…")
/// would regress the invariant.
#[test]
fn user_owner_still_cannot_merge_own_branch_to_main() {
    let err = validate_premerge("alice", "alice/abc12345", "main").expect_err("must reject");
    assert!(matches!(err, PreMergeError::UserBranchToMain { .. }));
}

/// `release/<v>` → `main` is the Release Manager's legal path and must
/// remain accepted after this validator lands — otherwise the Release
/// epic (rel-*) breaks.
#[test]
fn release_to_main_still_accepted() {
    validate_merge("release/0.1.0", "main").expect("must accept");
}

/// `epic/<id>` → `release/<v>` is the Release-aware variant of case 4.
#[test]
fn epic_to_release_accepted() {
    validate_merge("epic/ryve-b8802f3b", "release/0.1.0").expect("must accept");
}

/// Reserved prefixes (`epic/`, `release/`, `crew/`, `merge/`) must be
/// classified as system branches — never as user branches whose actor
/// happens to be the reserved word. If this ever regresses, an attacker
/// (or a buggy caller) could push `epic/foo` and have it accepted as
/// actor `epic`, short `foo`, which would then route through actor-scoped
/// checks and trivially defeat the branching model.
#[test]
fn reserved_prefixes_are_system_branches() {
    for (name, is_user) in [
        ("main", false),
        ("epic/x", false),
        ("release/1.0.0", false),
        ("crew/cr-1", false),
        ("merge/abc", false),
        ("alice/abc", true),
    ] {
        let k = BranchKind::classify(name);
        let actually_user = matches!(k, BranchKind::User { .. });
        assert_eq!(
            actually_user, is_user,
            "branch `{name}` classification mismatch: got {k:?}, expected user={is_user}"
        );
    }
}

/// Any merge that is neither on the allow-list nor caught by the
/// user-branch-to-main rule must fall through to `IllegalTarget` so the
/// hook can present a uniform "not legal" error. Important pairs:
/// user → user (peer poach) and epic → epic (epic cross-merge) must
/// both be rejected.
#[test]
fn other_illegal_combinations_are_rejected() {
    assert!(matches!(
        validate_merge("alice/abc12345", "bob/def67890").unwrap_err(),
        PreMergeError::IllegalTarget { .. }
    ));
    assert!(matches!(
        validate_merge("epic/ryve-one", "epic/ryve-two").unwrap_err(),
        PreMergeError::IllegalTarget { .. }
    ));
    assert!(matches!(
        validate_merge("main", "epic/ryve-one").unwrap_err(),
        PreMergeError::IllegalTarget { .. }
    ));
}

/// Unknown / malformed branch names on either side of a proposed merge
/// must never be waved through — defense-in-depth against a hook being
/// invoked with a detached-HEAD ref string or an empty argument.
#[test]
fn unknown_or_empty_branches_rejected() {
    assert!(validate_merge("", "main").is_err());
    assert!(validate_merge("alice/abc12345", "").is_err());
    assert!(validate_merge("HEAD", "main").is_err());
    assert!(validate_merge("some-feature-branch", "main").is_err());
}
