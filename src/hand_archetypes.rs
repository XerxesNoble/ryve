// SPDX-License-Identifier: AGPL-3.0-or-later

//! Hand archetype tool-policy enforcement [sp-1471f46a].
//!
//! Ryve spawns Hand subprocesses under a capability archetype. Read-only
//! archetypes (Investigator — the Cartographer / Reviewer shape from
//! [`docs/HAND_CAPABILITIES.md`]) must not be able to modify files or run
//! destructive commands, regardless of what the agent subprocess tries.
//! Write-capable archetypes (standard Owner, Merger) keep their existing
//! write authority.
//!
//! Enforcement is **mechanical** — not a prompt instruction. At spawn
//! time, after the worktree is created but before the agent process is
//! launched, [`apply_tool_policy`] chmod's the worktree tree to
//! `0o444 / 0o555` when the archetype is read-only. Any write syscall the
//! agent attempts then fails at the kernel boundary with `EACCES`. A
//! misbehaving agent that ignores its prompt still cannot mutate the
//! tree.
//!
//! This satisfies the invariant recorded on spark ryve-8384b3cc:
//!
//! > A read-only archetype cannot modify any file inside the worktree
//! > regardless of what the agent subprocess tries. Gating is a
//! > mechanical check — either a filesystem policy (read-only mount,
//! > deny-write wrapper) or an agent-tool allow-list passed into the
//! > subprocess — not a prompt instruction.
//!
//! Non-goal (same spark): fine-grained per-path write ACLs. A read-only
//! worktree is a single coarse bit; callers that need per-file rules
//! should not use this module.

use std::path::Path;

use crate::hand_spawn::HandKind;

/// Write authority a Hand carries for the lifetime of its spark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    /// Hand may not modify any file in its worktree. Enforced by
    /// chmod'ing the worktree read-only before the subprocess starts.
    ReadOnly,
    /// Hand may modify files in its worktree — the pre-archetype
    /// behaviour, kept for every write-capable role so no regression
    /// lands on Owner / Merger Hands.
    WriteCapable,
}

/// Return the tool policy for a Hand kind. The single lookup the spawn
/// path goes through before launching a subprocess.
pub fn tool_policy_for(kind: HandKind) -> ToolPolicy {
    match kind {
        // Investigators sweep code read-only and deliver findings via
        // `ryve comment add` — see `compose_investigator_prompt` and the
        // Cartographer / Reviewer shapes in docs/HAND_CAPABILITIES.md.
        HandKind::Investigator => ToolPolicy::ReadOnly,
        // Release Manager commits to `release/*` branches and writes
        // artifact paths into the release row, so it is write-capable at
        // the filesystem level. The *command* allow-list (no hand/head
        // spawns, no comments outside release sparks) is a separate
        // policy enforced in the CLI — see [`enforce_action`].
        HandKind::ReleaseManager => ToolPolicy::WriteCapable,
        // Bug Hunter is a Triager + Surgeon hybrid: it must edit code to
        // land the fix and the regression test, so the filesystem policy
        // is write-capable. Its acceptance bar (failing test → passing
        // test + smallest possible diff) is scoped by the prompt, not by
        // a CLI allow-list — no kernel-level gate is mechanically
        // necessary. Spark ryve-e5688777 / [sp-1471f46a].
        HandKind::BugHunter => ToolPolicy::WriteCapable,
        // Performance Engineer is a Refactorer / Cartographer hybrid:
        // it profiles the hot path (Cartographer shape — read, measure,
        // report) and lands the fix (Refactorer shape — restructure
        // without changing behaviour). Its acceptance bar is a measured
        // delta vs a baseline, not a test pass, and recording that
        // delta requires editing code to introduce the improvement.
        // Filesystem policy is therefore write-capable; the "measured
        // delta" discipline is enforced by the prompt and the DONE
        // checklist, not by a kernel-level gate. Spark ryve-1c099466 /
        // [sp-1471f46a].
        HandKind::PerformanceEngineer => ToolPolicy::WriteCapable,
        // Architect is a read-only Reviewer/Cartographer hybrid: reads
        // code and design artifacts, posts recommendations as structured
        // comments. Never emits diffs, so the filesystem policy is
        // strictly read-only — enforced via the same chmod gate as
        // Investigator. Spark ryve-3f799949 / [sp-1471f46a].
        HandKind::Architect => ToolPolicy::ReadOnly,
        // Reviewer reads the author's branch and posts approve/reject
        // transitions + actionable comments. It never lands a diff —
        // rejections come back to the author for repair — so the
        // filesystem policy is strictly read-only, enforced via the
        // same chmod gate as Investigator/Architect.
        // Spark ryve-b0a369dc / [sp-f6259067].
        HandKind::Reviewer => ToolPolicy::ReadOnly,
        // Standard worker, Head (orchestrator), Merger (integrator) all
        // require worktree writes today. Changing these to read-only
        // would regress existing Crews.
        HandKind::Owner | HandKind::Head | HandKind::Merger => ToolPolicy::WriteCapable,
    }
}

/// Short, stable identifier for the archetype. Used in log lines so
/// gating failures are attributable to a specific archetype (acceptance
/// criterion (4) of spark ryve-8384b3cc).
pub fn archetype_id_for(kind: HandKind) -> &'static str {
    match kind {
        HandKind::Owner => "owner",
        HandKind::Head => "head",
        HandKind::Investigator => "investigator",
        HandKind::ReleaseManager => "release_manager",
        HandKind::BugHunter => "bug_hunter",
        HandKind::PerformanceEngineer => "performance_engineer",
        HandKind::Architect => "architect",
        HandKind::Reviewer => "reviewer",
        HandKind::Merger => "merger",
    }
}

// ─── Release Manager allow-list [sp-2a82fee7 / ryve-e6713ee7] ───────
//
// The Release Manager archetype's tool policy is an allow-list, not
// prompt prose. A caller identified as a Release Manager (via its
// `agent_sessions.session_label`) is restricted to:
//
//   - `ryve release *` subcommands (cut, add-epic, close, …).
//   - Read-only workgraph queries (`ryve spark list/show`, `ryve bond list`,
//     `ryve release list/show`, …) — these are never gated.
//   - Committing to the release branch it manages.
//   - `ryve comment add` **only** on sparks that are members of some
//     release (the "Atlas-only" channel — Atlas polls the release member
//     epics). Any other target is forbidden.
//
// Forbidden actions:
//
//   - Spawning a Hand (`ryve hand spawn`) or Head (`ryve head spawn`).
//   - Commenting on a non-release spark.
//   - Sending embers (broadcast signals that cross the Atlas-only
//     comms discipline).
//
// The enforcement is a pure function: pass in the caller's
// [`CallerArchetype`] + the [`Action`] they are about to perform; the
// function returns `Ok(())` or a typed error the CLI turns into a
// non-zero exit.

/// What archetype the caller of a CLI command runs under. Determined at
/// the CLI entry point by resolving `RYVE_HAND_SESSION_ID` →
/// `agent_sessions.session_label` through [`caller_archetype_for_label`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerArchetype {
    /// Ordinary Hand / Head / Merger / direct human CLI use — no
    /// archetype-level restrictions apply. Default for any session_label
    /// that is not a known restricted archetype.
    Unrestricted,
    /// Release Manager: narrow tool-policy allow-list. See the module
    /// header for the exact rules.
    ReleaseManager,
}

impl CallerArchetype {
    /// Short, stable identifier used in error messages so operators can
    /// grep logs by archetype.
    pub fn id(self) -> &'static str {
        match self {
            Self::Unrestricted => "unrestricted",
            Self::ReleaseManager => "release_manager",
        }
    }
}

/// Resolve the caller's archetype from a raw `session_label` value (as
/// stored on `agent_sessions.session_label`). Unknown or absent labels
/// map to [`CallerArchetype::Unrestricted`]. Pure function; no DB calls.
pub fn caller_archetype_for_label(label: Option<&str>) -> CallerArchetype {
    match label {
        Some("release_manager") => CallerArchetype::ReleaseManager,
        _ => CallerArchetype::Unrestricted,
    }
}

/// Categorise the CLI command the caller is about to perform. The enum
/// is deliberately coarse — each variant maps to an entry point where
/// the Release Manager's allow-list differs from the unrestricted
/// default. Extending the allow-list later means adding a variant here
/// and a match arm in [`enforce_action`].
#[derive(Debug, Clone)]
pub enum Action<'a> {
    /// `ryve hand spawn ...` — forbidden for Release Managers (they must
    /// not spawn workers; only Atlas spawns them).
    SpawnHand,
    /// `ryve head spawn ...` — forbidden for Release Managers.
    SpawnHead,
    /// `ryve comment add <spark_id> ...`. `is_release_spark` tells
    /// [`enforce_action`] whether the target spark is a member of a
    /// release (computed by the caller via
    /// [`crate::hand_archetypes`] consumers, typically
    /// `release_repo::is_release_member`).
    CommentAdd {
        spark_id: &'a str,
        is_release_spark: bool,
    },
    /// `ryve ember send ...` — forbidden for Release Managers (embers
    /// broadcast outside the Atlas channel).
    EmberSend,
}

/// Error returned by [`enforce_action`] when a caller's archetype
/// forbids the requested action. Carries the archetype id and a short
/// reason so the CLI can print an operator-friendly message.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    /// Caller is a restricted archetype that forbids spawning new
    /// Hands / Heads. The Release Manager (Atlas-only comms) is the
    /// motivating case.
    #[error("archetype '{archetype}': spawning a {target} is forbidden by tool policy")]
    Spawn {
        archetype: &'static str,
        target: &'static str,
    },
    /// Caller is a restricted archetype whose comment targets are
    /// limited to release member sparks.
    #[error(
        "archetype '{archetype}': `ryve comment add {spark_id}` forbidden — \
         target is not a release member spark (comments must flow to Atlas \
         via release sparks)"
    )]
    Comment {
        archetype: &'static str,
        spark_id: String,
    },
    /// Caller is a restricted archetype that must not broadcast embers.
    #[error("archetype '{archetype}': `ryve ember send` forbidden by tool policy")]
    Ember { archetype: &'static str },
}

/// Check whether `caller` is allowed to perform `action`. Pure function;
/// every branch is deterministic from its inputs so the unit tests can
/// exhaustively cover the Release Manager allow-list.
///
/// Returns `Ok(())` for any unrestricted caller, and for restricted
/// callers whose action is on the allow-list. Returns a typed
/// [`PolicyError`] otherwise — the CLI wraps that into a non-zero exit
/// so no disallowed action ever makes it into the workgraph.
pub fn enforce_action(caller: CallerArchetype, action: &Action<'_>) -> Result<(), PolicyError> {
    match (caller, action) {
        // Unrestricted callers — everybody else in the workshop today —
        // keep their existing behaviour unchanged.
        (CallerArchetype::Unrestricted, _) => Ok(()),

        // Release Manager: the Atlas-only comms archetype.
        (CallerArchetype::ReleaseManager, Action::SpawnHand) => Err(PolicyError::Spawn {
            archetype: CallerArchetype::ReleaseManager.id(),
            target: "hand",
        }),
        (CallerArchetype::ReleaseManager, Action::SpawnHead) => Err(PolicyError::Spawn {
            archetype: CallerArchetype::ReleaseManager.id(),
            target: "head",
        }),
        (
            CallerArchetype::ReleaseManager,
            Action::CommentAdd {
                spark_id,
                is_release_spark,
            },
        ) => {
            if *is_release_spark {
                Ok(())
            } else {
                Err(PolicyError::Comment {
                    archetype: CallerArchetype::ReleaseManager.id(),
                    spark_id: (*spark_id).to_string(),
                })
            }
        }
        (CallerArchetype::ReleaseManager, Action::EmberSend) => Err(PolicyError::Ember {
            archetype: CallerArchetype::ReleaseManager.id(),
        }),
    }
}

/// Apply `policy` to `worktree_path`.
///
/// For [`ToolPolicy::ReadOnly`]: recursively chmod every regular file to
/// `0o444` and every directory to `0o555`. Directories keep their `x`
/// bit so they stay traversable for reads; files lose their `w` bit so
/// any write syscall against the tree fails with `EACCES`. Symlinks
/// are left alone — their permission bits are not portable across
/// Unixes and are not the write surface we care about.
///
/// For [`ToolPolicy::WriteCapable`]: no-op; writes proceed as today.
///
/// On non-Unix platforms the Unix chmod logic is skipped. Ryve targets
/// macOS/Linux; adding Windows-native ACLs is out of scope.
///
/// Errors surface in the log with the archetype id so a gating failure
/// at spawn time is obvious to the operator.
pub fn apply_tool_policy(
    worktree_path: &Path,
    policy: ToolPolicy,
    archetype_id: &str,
) -> std::io::Result<()> {
    match policy {
        ToolPolicy::WriteCapable => Ok(()),
        ToolPolicy::ReadOnly => enforce_read_only(worktree_path, archetype_id),
    }
}

#[cfg(unix)]
fn enforce_read_only(worktree_path: &Path, archetype_id: &str) -> std::io::Result<()> {
    match make_read_only_recursive(worktree_path) {
        Ok(()) => {
            log::info!(
                "archetype '{archetype_id}': worktree {} marked read-only",
                worktree_path.display()
            );
            Ok(())
        }
        Err(e) => {
            log::error!(
                "archetype '{archetype_id}': failed to apply read-only policy to {}: {e}",
                worktree_path.display()
            );
            Err(e)
        }
    }
}

#[cfg(not(unix))]
fn enforce_read_only(worktree_path: &Path, archetype_id: &str) -> std::io::Result<()> {
    let _ = (worktree_path, archetype_id);
    Ok(())
}

#[cfg(unix)]
fn make_read_only_recursive(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let meta = std::fs::symlink_metadata(path)?;
    let ft = meta.file_type();
    if ft.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            make_read_only_recursive(&entry.path())?;
        }
        let mut perms = meta.permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(path, perms)?;
    } else if !ft.is_symlink() {
        let mut perms = meta.permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Restore write permissions across `path` recursively. Called from the
/// worktree-cleanup path so `git worktree remove` (and bare
/// `remove_dir_all`) can unlink files inside a tree that was locked
/// down by [`apply_tool_policy`]. A no-op on non-Unix, and a no-op for
/// trees that were never locked (write-capable Hands end up touching
/// the same paths with their existing perms).
pub fn unlock_worktree(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        restore_writable_recursive(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(unix)]
fn restore_writable_recursive(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        return Ok(());
    }
    // Add the owner's write bit; xor'ing a dir also keeps its x bit so
    // traversal still works. We preserve other bits (group/other) as-is
    // because we don't know what the user's umask was originally.
    let mut perms = meta.permissions();
    let mode = perms.mode();
    let new_mode = if ft.is_dir() {
        mode | 0o700
    } else {
        mode | 0o600
    };
    perms.set_mode(new_mode);
    std::fs::set_permissions(path, perms)?;
    if ft.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            restore_writable_recursive(&entry.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn investigator_is_read_only() {
        assert_eq!(
            tool_policy_for(HandKind::Investigator),
            ToolPolicy::ReadOnly
        );
    }

    #[test]
    fn owner_and_head_and_merger_are_write_capable() {
        assert_eq!(tool_policy_for(HandKind::Owner), ToolPolicy::WriteCapable);
        assert_eq!(tool_policy_for(HandKind::Head), ToolPolicy::WriteCapable);
        assert_eq!(tool_policy_for(HandKind::Merger), ToolPolicy::WriteCapable);
    }

    #[test]
    fn release_manager_is_write_capable() {
        // The RM commits to its release branch and writes artifact
        // paths back into the release row, so the *filesystem* policy
        // is write-capable. Its command-level allow-list is enforced
        // separately via `enforce_action`.
        assert_eq!(
            tool_policy_for(HandKind::ReleaseManager),
            ToolPolicy::WriteCapable
        );
    }

    /// Invariant (spark ryve-e5688777 / [sp-1471f46a]): Bug Hunter must
    /// be write-capable — its acceptance bar is a failing-then-passing
    /// test plus the smallest possible diff, which requires editing
    /// code. A bug that flipped BugHunter to ReadOnly would block every
    /// fix from landing. Regression guard.
    #[test]
    fn bug_hunter_is_write_capable() {
        assert_eq!(
            tool_policy_for(HandKind::BugHunter),
            ToolPolicy::WriteCapable
        );
    }

    /// Invariant (spark ryve-1c099466 / [sp-1471f46a]): Performance
    /// Engineer is write-capable — its acceptance bar is a measured
    /// delta vs a baseline, and recording that delta requires landing
    /// the fix. A bug that flipped PerformanceEngineer to ReadOnly
    /// would make the archetype useless. Regression guard.
    #[test]
    fn performance_engineer_is_write_capable() {
        assert_eq!(
            tool_policy_for(HandKind::PerformanceEngineer),
            ToolPolicy::WriteCapable
        );
    }

    #[test]
    fn archetype_id_is_stable_per_kind() {
        // Log attribution depends on these being stable — every kind
        // lands on exactly one short id. Regression guard so a future
        // rename here cannot silently break log grepping.
        assert_eq!(archetype_id_for(HandKind::Owner), "owner");
        assert_eq!(archetype_id_for(HandKind::Head), "head");
        assert_eq!(archetype_id_for(HandKind::Investigator), "investigator");
        assert_eq!(
            archetype_id_for(HandKind::ReleaseManager),
            "release_manager"
        );
        assert_eq!(archetype_id_for(HandKind::BugHunter), "bug_hunter");
        assert_eq!(
            archetype_id_for(HandKind::PerformanceEngineer),
            "performance_engineer"
        );
        assert_eq!(archetype_id_for(HandKind::Architect), "architect");
        assert_eq!(archetype_id_for(HandKind::Reviewer), "reviewer");
        assert_eq!(archetype_id_for(HandKind::Merger), "merger");
    }

    /// Invariant (spark ryve-b0a369dc / [sp-f6259067]): the Reviewer Hand
    /// is strictly read-only — it reads the author's branch, approves or
    /// rejects the assignment, and posts comments. A reviewer that could
    /// silently edit the worktree would undermine the "second pair of
    /// eyes" contract. Regression guard.
    #[test]
    fn reviewer_is_read_only() {
        assert_eq!(tool_policy_for(HandKind::Reviewer), ToolPolicy::ReadOnly);
    }

    // ─── Release Manager allow-list [sp-2a82fee7] ──────────────────

    #[test]
    fn caller_archetype_maps_release_manager_label() {
        assert_eq!(
            caller_archetype_for_label(Some("release_manager")),
            CallerArchetype::ReleaseManager
        );
    }

    #[test]
    fn caller_archetype_defaults_to_unrestricted() {
        for label in [None, Some("hand"), Some("head"), Some("merger"), Some("")] {
            assert_eq!(
                caller_archetype_for_label(label),
                CallerArchetype::Unrestricted,
                "label {label:?} must not resolve to a restricted archetype",
            );
        }
    }

    /// Invariant (from spark ryve-e6713ee7): a Release Manager cannot
    /// spawn Hands or Heads. Attempting either must return a typed
    /// `PolicyError::Spawn` — the mechanical guard behind
    /// acceptance criterion (a) of the integration test.
    #[test]
    fn release_manager_cannot_spawn_hand_or_head() {
        let err = enforce_action(CallerArchetype::ReleaseManager, &Action::SpawnHand).unwrap_err();
        assert!(matches!(
            err,
            PolicyError::Spawn {
                archetype: "release_manager",
                target: "hand",
            }
        ));
        let err = enforce_action(CallerArchetype::ReleaseManager, &Action::SpawnHead).unwrap_err();
        assert!(matches!(
            err,
            PolicyError::Spawn {
                archetype: "release_manager",
                target: "head",
            }
        ));
    }

    /// Invariant (from spark ryve-e6713ee7): a Release Manager's comment
    /// channel is limited to release member sparks. Acceptance criterion
    /// (b) of the integration test depends on this.
    #[test]
    fn release_manager_comment_is_release_spark_only() {
        // Non-release target — rejected.
        let err = enforce_action(
            CallerArchetype::ReleaseManager,
            &Action::CommentAdd {
                spark_id: "ryve-unrelated",
                is_release_spark: false,
            },
        )
        .unwrap_err();
        match err {
            PolicyError::Comment {
                archetype,
                spark_id,
            } => {
                assert_eq!(archetype, "release_manager");
                assert_eq!(spark_id, "ryve-unrelated");
            }
            other => panic!("expected Comment, got {other:?}"),
        }

        // Release member — accepted.
        assert!(
            enforce_action(
                CallerArchetype::ReleaseManager,
                &Action::CommentAdd {
                    spark_id: "ryve-release-epic",
                    is_release_spark: true,
                },
            )
            .is_ok()
        );
    }

    /// Invariant (from spark ryve-e6713ee7): Release Manager cannot send
    /// embers — embers are broadcasts and breach the Atlas-only comms
    /// discipline.
    #[test]
    fn release_manager_cannot_send_embers() {
        let err = enforce_action(CallerArchetype::ReleaseManager, &Action::EmberSend).unwrap_err();
        assert!(matches!(
            err,
            PolicyError::Ember {
                archetype: "release_manager"
            }
        ));
    }

    /// The allow-list is positive-definition: `ryve release *` and
    /// read-only workgraph queries never reach [`enforce_action`] at
    /// all, and the RM variant has no restriction on them. Acceptance
    /// criterion (c) of the integration test — `ryve release list`
    /// under a RM session — is covered end-to-end by
    /// `tests/release_manager_hand.rs`.
    ///
    /// This unit test guards the complement: an action not gated by
    /// the RM allow-list (example: a future `Action::ReleaseCommand`)
    /// must be added here when introduced, so the allow-list stays
    /// mechanical rather than drifting into prose-only defaults.
    #[test]
    fn release_manager_allow_list_covers_every_gated_action() {
        // Every currently-gated action the CLI calls out at the
        // entry points. If a new `Action` variant is introduced, the
        // match below must be extended — the exhaustive match makes
        // the coverage obligation a build error instead of a silent
        // gap.
        let actions: Vec<Action<'_>> = vec![
            Action::SpawnHand,
            Action::SpawnHead,
            Action::CommentAdd {
                spark_id: "ryve-x",
                is_release_spark: false,
            },
            Action::EmberSend,
        ];
        for a in &actions {
            let result = enforce_action(CallerArchetype::ReleaseManager, a);
            match a {
                Action::SpawnHand | Action::SpawnHead | Action::EmberSend => {
                    assert!(result.is_err(), "RM must deny {a:?}");
                }
                Action::CommentAdd { .. } => {
                    assert!(result.is_err(), "RM must deny non-release comment");
                }
            }
        }
    }

    /// Regression guard for the default caller: every non-RM caller
    /// must pass through every action unchanged. A bug that widened the
    /// RM rules to another archetype would fail here.
    #[test]
    fn unrestricted_caller_is_never_gated() {
        for action in [
            Action::SpawnHand,
            Action::SpawnHead,
            Action::CommentAdd {
                spark_id: "ryve-anything",
                is_release_spark: false,
            },
            Action::EmberSend,
        ] {
            assert!(
                enforce_action(CallerArchetype::Unrestricted, &action).is_ok(),
                "unrestricted caller must pass {action:?}",
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn apply_read_only_blocks_writes_and_new_files() {
        let temp = unique_tempdir("ryve-tool-policy");
        std::fs::create_dir_all(temp.join("sub")).unwrap();
        let file = temp.join("sub").join("hello.txt");
        std::fs::write(&file, "hi").unwrap();

        apply_tool_policy(&temp, ToolPolicy::ReadOnly, "investigator").unwrap();

        let err = std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "read-only worktree must reject writes to existing files; got {err:?}"
        );

        let new_file = temp.join("sub").join("new.txt");
        let err = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&new_file)
            .unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "read-only worktree must reject new-file creation; got {err:?}"
        );

        restore_writable(&temp);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[cfg(unix)]
    #[test]
    fn write_capable_is_noop_and_writes_still_work() {
        let temp = unique_tempdir("ryve-tool-policy-wc");
        let file = temp.join("writable.txt");
        std::fs::write(&file, "hi").unwrap();

        apply_tool_policy(&temp, ToolPolicy::WriteCapable, "owner").unwrap();

        std::fs::write(&file, "still writable").expect("write-capable must stay writable");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "still writable");

        let _ = std::fs::remove_dir_all(&temp);
    }

    fn unique_tempdir(prefix: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[cfg(unix)]
    fn restore_writable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_dir() {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = std::fs::set_permissions(path, perms);
                if let Ok(entries) = std::fs::read_dir(path) {
                    for entry in entries.flatten() {
                        restore_writable(&entry.path());
                    }
                }
            } else if !meta.file_type().is_symlink() {
                let mut perms = meta.permissions();
                perms.set_mode(0o644);
                let _ = std::fs::set_permissions(path, perms);
            }
        }
    }
}
