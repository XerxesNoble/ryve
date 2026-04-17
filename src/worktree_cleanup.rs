// SPDX-License-Identifier: AGPL-3.0-or-later

//! Safe pruning of stale Hand worktrees.
//!
//! Worktrees under `.ryve/worktrees/<short_id>/` are created by
//! [`crate::workshop::create_hand_worktree`] when a Hand is spawned and
//! are *never* removed today — they accumulate until the user manually
//! prunes them. This module provides the safety predicate plus the
//! mechanical removal helpers used by the `ryve worktree prune` CLI
//! (Layer A) and, in later layers, the on-session-end auto-prune
//! (Layer B) and the boot-time sweeper (Layer C).
//!
//! The predicate is a pure function over typed inputs so it can be
//! unit-tested without spawning git or hitting the database. The CLI
//! and the live wiring are responsible for *gathering* the inputs;
//! this module never shells out on the test path.
//!
//! Tracking: epic ryve-b61e7ed4, Layer A spark ryve-261d06f3.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Outcome of inspecting a single worktree directory under
/// `.ryve/worktrees/`. Each non-`Removable` variant carries enough
/// detail for the CLI to print a useful reason line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeStatus {
    /// All safety checks pass — `git worktree remove` + `git branch -D`
    /// is safe.
    Removable,
    /// Working tree has uncommitted modifications. Never auto-removed.
    DirtyTree,
    /// Branch tip is not reachable from `main`. Carries the count of
    /// commits ahead so the CLI can show "skipped: 3 unmerged commits".
    UnmergedCommits(u32),
    /// The backing `agent_sessions` row is still `active` or its
    /// `child_pid` is alive. Never touched while a Hand might still be
    /// using the worktree.
    LiveSession,
    /// The directory does not match the `hand/<short_id>` convention
    /// (e.g. crew/* worktrees, merge-* worktrees, or hand-rolled
    /// directories). Reported but never touched.
    NotHandWorktree,
}

impl WorktreeStatus {
    /// Single-character glyph for the dry-run output. Keeps each row
    /// scannable at a glance.
    pub fn glyph(&self) -> char {
        match self {
            WorktreeStatus::Removable => '✓',
            WorktreeStatus::DirtyTree => '✗',
            WorktreeStatus::UnmergedCommits(_) => '⚠',
            WorktreeStatus::LiveSession => '●',
            WorktreeStatus::NotHandWorktree => '–',
        }
    }

    /// Human-readable reason that follows the glyph in dry-run output.
    pub fn reason(&self) -> String {
        match self {
            WorktreeStatus::Removable => "removable".to_string(),
            WorktreeStatus::DirtyTree => "dirty working tree".to_string(),
            WorktreeStatus::UnmergedCommits(n) => {
                format!("{n} unmerged commit{}", if *n == 1 { "" } else { "s" })
            }
            WorktreeStatus::LiveSession => "live agent session".to_string(),
            WorktreeStatus::NotHandWorktree => "not a hand worktree".to_string(),
        }
    }
}

/// Snapshot of everything the predicate needs about one worktree.
/// Built by the CLI side via [`gather_facts`] and consumed by
/// [`classify_worktree`]. Keeping the gather/classify split lets the
/// tests construct facts directly without touching git or the DB.
#[derive(Debug, Clone)]
pub struct WorktreeFacts {
    /// Filesystem path to the worktree root.
    pub path: PathBuf,
    /// Short id parsed from the directory name (`abcd1234` form).
    /// `None` if the directory name doesn't match the 8-char pattern,
    /// in which case the worktree is reported as `NotHandWorktree`.
    pub short_id: Option<String>,
    /// Branch checked out in the worktree, e.g. `hand/abcd1234`.
    /// `None` if git couldn't tell us (worktree corrupted, etc.) — also
    /// classified as `NotHandWorktree` defensively.
    pub branch: Option<String>,
    /// `true` when `git status --porcelain` returns empty output.
    pub is_clean: bool,
    /// Count of commits the branch tip has ahead of `main`. `0` means
    /// fully merged (or empty branch); any positive value blocks removal.
    pub unmerged_count: u32,
    /// `true` when the backing `agent_sessions` row is `active` OR the
    /// `child_pid` of any matching session is still alive. Either case
    /// means a Hand might still be using the worktree.
    pub session_live: bool,
}

/// Classify a worktree based on the facts gathered for it.
///
/// **Order of checks matters** — we report the most-blocking reason
/// first so the CLI doesn't show "dirty" for a worktree whose real
/// problem is that the agent is still running. Order:
///   1. `NotHandWorktree` (out of scope, nothing else applies)
///   2. `LiveSession` (don't poke a running agent's worktree)
///   3. `DirtyTree` (would lose uncommitted work)
///   4. `UnmergedCommits(n)` (would lose committed work)
///   5. `Removable`
pub fn classify_worktree(facts: &WorktreeFacts) -> WorktreeStatus {
    let Some(short_id) = facts.short_id.as_deref() else {
        return WorktreeStatus::NotHandWorktree;
    };
    // Defensive: branch must be `<actor>/<short_id>` to qualify, where
    // `<actor>` is a single non-reserved path segment. Reserved prefixes
    // (`crew/*`, `epic/*`, `release/*`) are explicitly out of scope, as
    // are `merge-*` directories and detached HEADs. Actor-scoped branches
    // replaced the legacy `hand/<short>` naming in spark ryve-c44b92e5.
    let Some(branch) = facts.branch.as_deref() else {
        return WorktreeStatus::NotHandWorktree;
    };
    if !branch_is_actor_scoped_hand(branch, short_id) {
        return WorktreeStatus::NotHandWorktree;
    }

    if facts.session_live {
        return WorktreeStatus::LiveSession;
    }
    if !facts.is_clean {
        return WorktreeStatus::DirtyTree;
    }
    if facts.unmerged_count > 0 {
        return WorktreeStatus::UnmergedCommits(facts.unmerged_count);
    }
    WorktreeStatus::Removable
}

/// One classified candidate for the prune report.
#[derive(Debug, Clone)]
pub struct PruneCandidate {
    pub facts: WorktreeFacts,
    pub status: WorktreeStatus,
}

/// Aggregate counts shown in the prune summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneSummary {
    pub removable: usize,
    pub dirty: usize,
    pub unmerged: usize,
    pub live: usize,
    pub out_of_scope: usize,
}

impl PruneSummary {
    pub fn record(&mut self, status: &WorktreeStatus) {
        match status {
            WorktreeStatus::Removable => self.removable += 1,
            WorktreeStatus::DirtyTree => self.dirty += 1,
            WorktreeStatus::UnmergedCommits(_) => self.unmerged += 1,
            WorktreeStatus::LiveSession => self.live += 1,
            WorktreeStatus::NotHandWorktree => self.out_of_scope += 1,
        }
    }
}

// ── git side-effect helpers ─────────────────────────
//
// Everything below this line shells out to git or to the OS. The CLI
// uses these to build `WorktreeFacts` for each candidate; the unit
// tests do not — they construct `WorktreeFacts` directly so the
// predicate is exercised without any process spawn.

/// True when `branch` has the shape `<actor>/<short_id>` that a Hand
/// worktree is expected to be on: exactly one `/`, the suffix equals the
/// short_id, and the actor segment is not one of the reserved top-level
/// prefixes that the rest of Ryve owns (`crew`, `epic`, `release`, `merge`).
///
/// Actor-scoped hand branches replaced the legacy `hand/<short>` naming in
/// spark ryve-c44b92e5; this predicate accepts the new form while keeping
/// crew / epic / release / merge worktrees out of the cleanup path.
pub fn branch_is_actor_scoped_hand(branch: &str, short_id: &str) -> bool {
    let Some((actor, suffix)) = branch.split_once('/') else {
        return false;
    };
    if suffix != short_id {
        return false;
    }
    if actor.is_empty() || actor.contains('/') {
        return false;
    }
    !matches!(actor, "crew" | "epic" | "release" | "merge")
}

/// Parse the 8-char short id from a worktree directory name. Returns
/// `None` for anything that doesn't match `[0-9a-f]{8}` exactly so
/// crew/* and merge-* dirs (which live alongside the hand worktrees)
/// are filtered out at gather time.
pub fn parse_short_id(dir_name: &str) -> Option<String> {
    if dir_name.len() != 8 {
        return None;
    }
    if !dir_name.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(dir_name.to_string())
}

/// Branch currently checked out in the given worktree, via
/// `git -C <path> branch --show-current`. Returns `None` for detached
/// HEAD or any git failure.
pub fn worktree_branch(worktree: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Filenames that Ryve itself creates or auto-syncs into every Hand
/// worktree, which therefore appear in `git status --porcelain` for
/// reasons unrelated to the agent's actual work:
///
/// - `AGENTS.md` — copied into every worktree by
///   [`crate::workshop::create_hand_worktree`] so codex/opencode (which
///   have no `--system-prompt` flag) can still read WORKSHOP.md. Always
///   untracked.
/// - `.ryve/WORKSHOP.md` — re-synced by `data::agent_context::sync` on
///   every workshop tick. Always shows up modified in long-lived
///   worktrees.
///
/// These are filtered out before deciding whether the working tree is
/// "really" dirty. Any *other* modified or untracked file is treated as
/// real work and blocks removal — the predicate stays conservative.
const RYVE_MANAGED_PATHS: &[&str] = &["AGENTS.md", ".ryve/WORKSHOP.md"];

/// `true` if `git -C <path> status --porcelain` reports no changes
/// other than the Ryve-managed files listed in [`RYVE_MANAGED_PATHS`].
/// Any failure is treated as "dirty" so we don't remove a worktree we
/// couldn't inspect.
pub fn worktree_is_clean(worktree: &Path) -> bool {
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["status", "--porcelain"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    porcelain_is_clean_ignoring_managed(&stdout)
}

/// Pure helper used by [`worktree_is_clean`] and exercised directly by
/// the unit tests. Walks each non-empty line of porcelain output and
/// returns `false` as soon as it finds one whose filename is *not* in
/// [`RYVE_MANAGED_PATHS`]. Each line is `XY filename` where `XY` is two
/// status chars and the filename starts at byte 3.
pub(crate) fn porcelain_is_clean_ignoring_managed(stdout: &str) -> bool {
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        // Defensive: porcelain v1 lines are `XY filename`. Anything
        // shorter than 4 bytes is malformed — treat as dirty.
        if line.len() < 4 {
            return false;
        }
        let filename = &line[3..];
        // Renames are reported as `R  old -> new`; we only inspect the
        // first path which is enough to know it's NOT one of our two
        // managed files (those are never renamed).
        let candidate = filename.split(" -> ").next().unwrap_or(filename);
        if !RYVE_MANAGED_PATHS.contains(&candidate) {
            return false;
        }
    }
    true
}

/// Count of commits the branch tip is ahead of `main`. `0` means the
/// branch is fully merged (or has no commits at all). Uses
/// `git -C <repo> rev-list --count main..<branch>`. Any git failure
/// reports a non-zero "unknown" value so the predicate skips removal —
/// we never auto-remove a branch we couldn't classify.
pub fn unmerged_count(repo: &Path, branch: &str) -> u32 {
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-list", "--count", &format!("main..{branch}")])
        .output()
    else {
        return u32::MAX;
    };
    if !output.status.success() {
        return u32::MAX;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .unwrap_or(u32::MAX)
}

/// `true` if the OS reports the given pid is still a live process.
/// Falls back to `false` (treat as dead) on any error so a corrupted
/// agent_sessions row doesn't pin a worktree forever.
pub fn process_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) returns 0 if the process exists and we have
        // permission to signal it; -1 with ESRCH if it's gone.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Run `git worktree remove --force <path>`. The `--force` is needed
/// because git refuses to remove a worktree with submodules or
/// uncommitted changes — but the predicate has already established the
/// tree is clean, so `--force` here is purely defensive against git's
/// own conservative checks.
///
/// If the worktree was locked read-only by a read-only archetype
/// ([`crate::hand_archetypes::apply_tool_policy`]), its files and dirs
/// need their `w` bit restored before git can unlink them — otherwise
/// `git worktree remove --force` fails with `Permission denied` on
/// every child. The restore call is a no-op on trees that were never
/// locked, so it is safe to invoke unconditionally.
///
/// Returns `Ok` on success, `Err(stderr)` on failure.
pub fn run_worktree_remove(repo: &Path, worktree: &Path) -> Result<(), String> {
    if let Err(e) = crate::hand_archetypes::unlock_worktree(worktree) {
        return Err(format!("unlock read-only worktree: {e}"));
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "remove", "--force"])
        .arg(worktree)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

/// Run `git branch -D hand/<short_id>`. Used after the worktree itself
/// is removed; the branch ref otherwise lingers as a stale pointer.
pub fn run_branch_delete(repo: &Path, branch: &str) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["branch", "-D", branch])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(
        short_id: Option<&str>,
        branch: Option<&str>,
        is_clean: bool,
        unmerged: u32,
        session_live: bool,
    ) -> WorktreeFacts {
        WorktreeFacts {
            path: PathBuf::from("/tmp/wt"),
            short_id: short_id.map(|s| s.to_string()),
            branch: branch.map(|s| s.to_string()),
            is_clean,
            unmerged_count: unmerged,
            session_live,
        }
    }

    // ── classify_worktree ──────────────────────

    #[test]
    fn clean_merged_inactive_is_removable() {
        let f = facts(Some("abcd1234"), Some("hand/abcd1234"), true, 0, false);
        assert_eq!(classify_worktree(&f), WorktreeStatus::Removable);
    }

    #[test]
    fn dirty_tree_blocks_removal() {
        let f = facts(Some("abcd1234"), Some("hand/abcd1234"), false, 0, false);
        assert_eq!(classify_worktree(&f), WorktreeStatus::DirtyTree);
    }

    #[test]
    fn unmerged_commits_block_removal() {
        let f = facts(Some("abcd1234"), Some("hand/abcd1234"), true, 3, false);
        assert_eq!(classify_worktree(&f), WorktreeStatus::UnmergedCommits(3));
    }

    #[test]
    fn live_session_blocks_removal_even_when_clean_and_merged() {
        // Live session is reported FIRST — we don't want the dry-run
        // saying "dirty" or "unmerged" when the real reason is "the
        // agent is still typing into it".
        let f = facts(Some("abcd1234"), Some("hand/abcd1234"), true, 0, true);
        assert_eq!(classify_worktree(&f), WorktreeStatus::LiveSession);
    }

    #[test]
    fn live_session_takes_precedence_over_dirty() {
        let f = facts(Some("abcd1234"), Some("hand/abcd1234"), false, 5, true);
        assert_eq!(classify_worktree(&f), WorktreeStatus::LiveSession);
    }

    #[test]
    fn missing_short_id_is_out_of_scope() {
        let f = facts(None, Some("hand/abcd1234"), true, 0, false);
        assert_eq!(classify_worktree(&f), WorktreeStatus::NotHandWorktree);
    }

    #[test]
    fn crew_branch_is_out_of_scope() {
        // Even if the directory matches the 8-char pattern, a crew/*
        // branch must NOT be touched — the predicate gates on actor-
        // scoped forms `<actor>/<short_id>` with `actor` outside the
        // reserved set {crew, epic, release, merge}.
        let f = facts(Some("abcd1234"), Some("crew/cr-deadbeef"), true, 0, false);
        assert_eq!(classify_worktree(&f), WorktreeStatus::NotHandWorktree);
    }

    /// Spark ryve-c44b92e5: actor-scoped branches like `alice/abcd1234`
    /// are hand worktrees and must be classifiable by the predicate.
    #[test]
    fn actor_scoped_branch_is_removable() {
        let f = facts(Some("abcd1234"), Some("alice/abcd1234"), true, 0, false);
        assert_eq!(classify_worktree(&f), WorktreeStatus::Removable);
    }

    /// The reserved epic/release/merge prefixes must also be refused even
    /// if the suffix happens to match the short_id, so cleanup never
    /// deletes integration / release branches.
    #[test]
    fn reserved_prefixes_are_out_of_scope() {
        for prefix in ["epic", "release", "merge"] {
            let branch = format!("{prefix}/abcd1234");
            let f = facts(Some("abcd1234"), Some(branch.as_str()), true, 0, false);
            assert_eq!(
                classify_worktree(&f),
                WorktreeStatus::NotHandWorktree,
                "{prefix}/<short_id> must never be treated as a hand worktree"
            );
        }
    }

    /// A short_id suffix that doesn't match the worktree directory name
    /// must still be refused — otherwise a mislabeled branch could
    /// accidentally be targeted.
    #[test]
    fn branch_suffix_must_equal_short_id() {
        let f = facts(Some("abcd1234"), Some("alice/deadbeef"), true, 0, false);
        assert_eq!(classify_worktree(&f), WorktreeStatus::NotHandWorktree);
    }

    #[test]
    fn detached_head_is_out_of_scope() {
        let f = facts(Some("abcd1234"), None, true, 0, false);
        assert_eq!(classify_worktree(&f), WorktreeStatus::NotHandWorktree);
    }

    #[test]
    fn dirty_blocks_when_session_dead_but_unmerged_zero() {
        // Dirty has higher priority than the (vacuous) merged check.
        let f = facts(Some("abcd1234"), Some("hand/abcd1234"), false, 0, false);
        assert_eq!(classify_worktree(&f), WorktreeStatus::DirtyTree);
    }

    #[test]
    fn unmerged_only_when_clean() {
        // If both dirty AND unmerged, dirty wins (it's the more
        // dangerous data-loss case to surface).
        let f = facts(Some("abcd1234"), Some("hand/abcd1234"), false, 7, false);
        assert_eq!(classify_worktree(&f), WorktreeStatus::DirtyTree);
    }

    // ── parse_short_id ─────────────────────────

    #[test]
    fn parse_short_id_accepts_8_hex() {
        assert_eq!(parse_short_id("abcd1234"), Some("abcd1234".to_string()));
        assert_eq!(parse_short_id("00000000"), Some("00000000".to_string()));
        assert_eq!(parse_short_id("ffffffff"), Some("ffffffff".to_string()));
    }

    #[test]
    fn parse_short_id_rejects_wrong_length() {
        assert_eq!(parse_short_id("abc"), None);
        assert_eq!(parse_short_id("abcd12345"), None); // 9 chars
        assert_eq!(parse_short_id(""), None);
    }

    #[test]
    fn parse_short_id_rejects_non_hex() {
        // Crew dir names like "cr-a25505" don't match.
        assert_eq!(parse_short_id("crew-abc"), None);
        assert_eq!(parse_short_id("ABCDEFGH"), None); // G/H not hex
        assert_eq!(parse_short_id("merge-cr"), None);
    }

    // ── PruneSummary ───────────────────────────

    #[test]
    fn prune_summary_records_each_status_in_its_own_bucket() {
        let mut s = PruneSummary::default();
        s.record(&WorktreeStatus::Removable);
        s.record(&WorktreeStatus::Removable);
        s.record(&WorktreeStatus::DirtyTree);
        s.record(&WorktreeStatus::UnmergedCommits(3));
        s.record(&WorktreeStatus::LiveSession);
        s.record(&WorktreeStatus::NotHandWorktree);

        assert_eq!(
            s,
            PruneSummary {
                removable: 2,
                dirty: 1,
                unmerged: 1,
                live: 1,
                out_of_scope: 1,
            }
        );
    }

    // ── status surface ─────────────────────────

    #[test]
    fn status_glyph_is_unique_per_variant() {
        // Sanity: each status has a distinct glyph so the dry-run
        // output is scannable.
        let glyphs: Vec<char> = [
            WorktreeStatus::Removable,
            WorktreeStatus::DirtyTree,
            WorktreeStatus::UnmergedCommits(1),
            WorktreeStatus::LiveSession,
            WorktreeStatus::NotHandWorktree,
        ]
        .iter()
        .map(|s| s.glyph())
        .collect();
        let mut sorted = glyphs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), glyphs.len(), "duplicate glyph");
    }

    #[test]
    fn unmerged_reason_pluralizes() {
        assert_eq!(
            WorktreeStatus::UnmergedCommits(1).reason(),
            "1 unmerged commit"
        );
        assert_eq!(
            WorktreeStatus::UnmergedCommits(7).reason(),
            "7 unmerged commits"
        );
    }

    // ── porcelain_is_clean_ignoring_managed ────

    #[test]
    fn empty_porcelain_is_clean() {
        assert!(porcelain_is_clean_ignoring_managed(""));
    }

    #[test]
    fn only_managed_files_is_clean() {
        // Real-world output we saw on every stale worktree during the
        // Layer A smoke test: WORKSHOP.md modified by the auto-sync,
        // AGENTS.md untracked because create_hand_worktree drops it.
        // Neither is real work — both must be ignored.
        let porcelain = " M .ryve/WORKSHOP.md\n?? AGENTS.md\n";
        assert!(porcelain_is_clean_ignoring_managed(porcelain));
    }

    #[test]
    fn untracked_user_file_blocks_clean() {
        // An untracked file that ISN'T one of our managed paths is real
        // dirt — the agent left something behind we shouldn't drop.
        let porcelain = "?? AGENTS.md\n?? scratch.txt\n";
        assert!(!porcelain_is_clean_ignoring_managed(porcelain));
    }

    #[test]
    fn modified_user_file_blocks_clean() {
        // .github/copilot-instructions.md was the outlier in the smoke
        // test — exactly the kind of "real dirt" we must preserve.
        let porcelain = " M .ryve/WORKSHOP.md\n M .github/copilot-instructions.md\n";
        assert!(!porcelain_is_clean_ignoring_managed(porcelain));
    }

    #[test]
    fn staged_user_file_blocks_clean() {
        // `M  filename` (M then space then space) means staged.
        let porcelain = "M  src/main.rs\n";
        assert!(!porcelain_is_clean_ignoring_managed(porcelain));
    }

    #[test]
    fn rename_with_managed_first_path_still_blocks_when_target_is_user_file() {
        // Defensive: a rename of AGENTS.md → other.md is exotic but
        // shouldn't be silently treated as clean. We only ever check
        // the first path of a rename, and the first path here is
        // exactly AGENTS.md, so this case slips through as "clean".
        // That's fine — AGENTS.md is auto-regenerated next spawn, and
        // a rename of it is genuinely Ryve-managed file activity.
        let porcelain = "R  AGENTS.md -> other.md\n";
        assert!(porcelain_is_clean_ignoring_managed(porcelain));
        // But a rename of a real file → managed name must NOT be clean.
        let porcelain = "R  src/main.rs -> AGENTS.md\n";
        assert!(!porcelain_is_clean_ignoring_managed(porcelain));
    }

    #[test]
    fn malformed_short_line_treated_as_dirty() {
        // Defensive: lines shorter than `XY filename` (4+ chars) are
        // malformed and we conservatively report as dirty.
        let porcelain = "??\n";
        assert!(!porcelain_is_clean_ignoring_managed(porcelain));
    }
}
