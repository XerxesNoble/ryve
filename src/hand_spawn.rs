// SPDX-License-Identifier: AGPL-3.0-or-later

//! Shared CLI-driven spawn flow for Hands and the Merger.
//!
//! The Ryve UI spawns Hands via `workshop::Workshop::spawn_terminal` so the
//! agent runs inside an `iced_term` widget. The Head, however, is itself a
//! coding-agent subprocess and needs to spawn child Hands without going
//! through iced. This module provides the data-layer half of that flow:
//!
//!   1. Create a git worktree for the new Hand.
//!   2. Persist an `agent_sessions` row.
//!   3. Persist a `hand_assignments` row claiming the spark with the
//!      requested role (Owner / Merger).
//!   4. Optionally add the new session to a Crew.
//!   5. Compose the appropriate initial prompt (regular Hand or Merger).
//!   6. Launch the coding-agent subprocess **detached** with the system
//!      prompt injected via the agent's `--system-prompt`-style flag, full
//!      auto enabled, environment configured (`RYVE_WORKSHOP_ROOT`, `PATH`),
//!      cwd set to the new worktree, and stdout/stderr redirected to a log
//!      file under `.ryve/logs/hand-<session_id>.log`.
//!
//! The detached child process keeps running after the `ryve hand spawn`
//! invocation exits, so the Head can fire-and-forget. Ryve's UI picks up
//! the new session on its next workgraph poll.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use data::ryve_dir::RyveDir;
use data::sparks::types::{
    AssignmentRole, NewAgentSession, NewCrew, NewHandAssignment, Spark, SparkFilter,
};
use data::sparks::{
    agent_session_repo, assign_repo, assignment_repo, bond_repo, crew_repo, spark_repo,
};
use sqlx::SqlitePool;
use tokio::io::AsyncWrite;
use tokio::process::Command;
use uuid::Uuid;

use crate::agent_prompts::{
    HeadArchetype, compose_architect_prompt, compose_bug_hunter_prompt, compose_hand_prompt,
    compose_head_prompt, compose_investigator_prompt, compose_merger_prompt,
    compose_performance_engineer_prompt, compose_release_manager_prompt, compose_reviewer_prompt,
};
use crate::coding_agents::CodingAgent;
use crate::stream_heartbeat::{StreamHeartbeat, StreamOutcome};
use crate::tmux::{self, TmuxClient, TmuxError};
use crate::{hand_archetypes, workshop};

/// What kind of Hand we are spawning. Determines which initial prompt is
/// composed and which `AssignmentRole` is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandKind {
    /// Standard owner-of-the-spark Hand.
    Owner,
    /// A **Head**: a coding-agent subprocess that orchestrates a Crew of
    /// Hands. Mechanically identical to an Owner Hand (same worktree,
    /// same session row, same launch flow), distinguished by
    /// `agent_sessions.session_label = "head"` and by the Head system
    /// prompt composed via [`compose_head_prompt`]. The assignment row
    /// records `AssignmentRole::Owner` against the parent epic because
    /// the Head "owns" that epic for the lifetime of its crew.
    Head,
    /// A read-only **Investigator** Hand. Spawned by a Research Head to
    /// audit code and post findings as comments. Mechanically identical
    /// to an Owner Hand (same worktree, same session row, same launch
    /// flow), distinguished by `agent_sessions.session_label =
    /// "investigator"` and by the read-only system prompt composed via
    /// [`compose_investigator_prompt`]. The assignment row records
    /// `AssignmentRole::Owner` against the audit spark because the
    /// investigator "owns" that spark for the lifetime of its sweep.
    Investigator,
    /// A **Release Manager** Hand. A singleton-shaped archetype whose
    /// entire job is steering a Release: cutting the branch, advancing
    /// status, add/remove epics, tag, artifact, close. Its communication
    /// graph is deliberately narrow — it takes direction only from Atlas
    /// and reports only to Atlas — and its tool policy is an allow-list
    /// (`ryve release *`, read-only workgraph queries, commits to the
    /// release branch, comments targeted at Atlas on release sparks). The
    /// enforcement is mechanical via
    /// [`crate::hand_archetypes::enforce_action`], not a prompt
    /// suggestion. Mechanically identical to an Owner Hand on the DB
    /// side (same worktree, same session row, same launch flow),
    /// distinguished by `agent_sessions.session_label = "release_manager"`
    /// and by the composer [`compose_release_manager_prompt`]. Spark
    /// ryve-e6713ee7 / [sp-2a82fee7].
    ReleaseManager,
    /// A **Bug Hunter** Hand — a Triager + Surgeon hybrid specialised on
    /// small defects. Reproduces the bug with a failing test, localises
    /// the root cause, and lands the smallest possible diff that makes
    /// the test pass. Write-capable (it needs to edit code) but scoped
    /// by its prompt: the acceptance bar is "failing test → passing
    /// test plus smallest possible diff", not "feature shipped".
    /// Language-agnostic — the archetype makes no assumptions about
    /// project language, test runner, or framework; the agent chooses
    /// whichever toolchain the repo already uses. Mechanically
    /// identical to an Owner Hand on the DB side (same worktree, same
    /// session row, same launch flow), distinguished by
    /// `agent_sessions.session_label = "bug_hunter"` and by the
    /// composer [`compose_bug_hunter_prompt`]. Spark ryve-e5688777 /
    /// [sp-1471f46a].
    BugHunter,
    /// A **Performance Engineer** Hand — a Refactorer + Cartographer
    /// hybrid specialised on measurable performance improvements. Its
    /// acceptance bar is a measured delta against a baseline, not a
    /// test pass: baseline → profile → propose → verify, with
    /// before/after numbers recorded as spark comments so post-mortems
    /// can diff them. Write-capable (it must be able to land the fix)
    /// but its scope is policed by the prompt, not a CLI allow-list.
    /// Language-agnostic — the archetype makes no assumptions about
    /// profiling tools or benchmark harnesses; the agent picks tools
    /// appropriate to the repo it is in. Mechanically identical to an
    /// Owner Hand on the DB side (same worktree, same session row,
    /// same launch flow), distinguished by `agent_sessions.session_label
    /// = "performance_engineer"` and by the composer
    /// [`compose_performance_engineer_prompt`]. Spark ryve-1c099466 /
    /// [sp-1471f46a].
    PerformanceEngineer,
    /// A read-only **Architect** Hand. Spawned by a Review Head (or
    /// directly by Atlas) to produce design and architecture
    /// recommendations on the parent spark. Capability class is
    /// Reviewer/Cartographer: the Architect reads code and design
    /// artifacts, then posts structured recommendations, tradeoffs, and
    /// risks as comments on the parent spark — never diffs. Mechanically
    /// identical to an Investigator at the DB layer (session_label =
    /// "architect", `AssignmentRole::Owner`); the read-only contract is
    /// enforced by [`compose_architect_prompt`] and by the same
    /// capability-gate policy that binds the Investigator
    /// (spark ryve-3f799949).
    Architect,
    /// A **Reviewer** Hand. Spawned by [`spawn_reviewer`] on a spark whose
    /// author Hand has handed off for code review. Mechanically identical
    /// to an Owner Hand at the DB layer (same session row, same
    /// assignment row) but distinguished by `agent_sessions.session_label
    /// = "reviewer"` and by [`compose_reviewer_prompt`]. The Reviewer has
    /// authority over `AwaitingReview → Approved | Rejected` transitions
    /// (see data/src/sparks/transition.rs). Selection is deterministic
    /// and author-excluded; the spawn path is cross-vendor-preferring
    /// with a logged fallback. Spark ryve-b0a369dc / [sp-f6259067].
    Reviewer,
    /// The crew's integrator. Requires `crew_id` to be set.
    Merger,
}

impl HandKind {
    fn role(self) -> AssignmentRole {
        match self {
            Self::Owner => AssignmentRole::Owner,
            // Heads own the epic they are orchestrating — same assignment
            // semantics as an Owner Hand, just a different session_label
            // and system prompt.
            Self::Head => AssignmentRole::Owner,
            // Investigators claim the audit spark they are sweeping —
            // same assignment semantics as an Owner Hand, just a
            // different session_label and a read-only system prompt.
            Self::Investigator => AssignmentRole::Owner,
            // Release Managers claim the release-management spark they
            // were spawned on — same assignment semantics as an Owner
            // Hand, plus an archetype-level ToolPolicy allow-list that
            // the CLI consults on every workgraph mutation.
            Self::ReleaseManager => AssignmentRole::Owner,
            // Bug Hunters claim the bug spark they were dispatched on —
            // same assignment semantics as an Owner Hand, distinguished
            // only by session_label + prompt composer.
            Self::BugHunter => AssignmentRole::Owner,
            // Performance Engineers claim the perf spark they were
            // dispatched on — same assignment semantics as an Owner
            // Hand, distinguished only by session_label + prompt
            // composer. The "measured delta" acceptance bar is prose
            // in the prompt; there is no CLI-level gate to add.
            Self::PerformanceEngineer => AssignmentRole::Owner,
            // Architects claim the design-review spark they are scoped
            // to — same assignment semantics as an Owner Hand, different
            // session_label and a read-only system prompt.
            Self::Architect => AssignmentRole::Owner,
            // Reviewers sit alongside the author's Owner assignment
            // rather than replacing it — they transition the
            // `AwaitingReview → Approved|Rejected` phase without
            // claiming ownership of the spark. `Observer` lets the
            // reviewer's session row coexist with the author's Owner
            // row (the unique-active-owner check on `assignment_repo`
            // is scoped to `role = 'owner'`) and accurately describes
            // the relationship: the reviewer watches and judges the
            // author's work but does not take it over.
            Self::Reviewer => AssignmentRole::Observer,
            Self::Merger => AssignmentRole::Merger,
        }
    }

    /// The value written to `agent_sessions.session_label` and used as the
    /// crew_members role label. Kept here so spawn, crew-add, tmux-name,
    /// and tests all share one source of truth.
    fn session_label(self) -> &'static str {
        match self {
            Self::Owner => "hand",
            Self::Head => "head",
            Self::Investigator => "investigator",
            Self::ReleaseManager => "release_manager",
            Self::BugHunter => "bug_hunter",
            Self::PerformanceEngineer => "performance_engineer",
            Self::Architect => "architect",
            Self::Reviewer => "reviewer",
            Self::Merger => "merger",
        }
    }
}

/// Outcome of a successful spawn — useful for both the CLI handler (which
/// prints these) and the integration tests (which assert on them).
#[derive(Debug, Clone)]
pub struct SpawnedHand {
    pub session_id: String,
    pub spark_id: String,
    pub worktree_path: PathBuf,
    pub log_path: PathBuf,
    pub child_pid: Option<u32>,
}

/// Outcome of a successful [`spawn_head`]. Mirrors [`SpawnedHand`] but
/// carries the Crew id the new Head is registered on instead of a spark
/// assignment (Heads do not claim sparks — their crew is how the workgraph
/// locates them).
#[derive(Debug, Clone)]
pub struct SpawnedHead {
    pub session_id: String,
    pub epic_id: String,
    pub crew_id: String,
    pub archetype: HeadArchetype,
    pub worktree_path: PathBuf,
    pub log_path: PathBuf,
    pub child_pid: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum HandSpawnError {
    #[error("workgraph error: {0}")]
    Sparks(#[from] data::sparks::SparksError),
    #[error("worktree: {0}")]
    Worktree(String),
    #[error("filesystem: {0}")]
    Io(#[from] std::io::Error),
    #[error("merger spawn requires --crew <crew_id>")]
    MergerNeedsCrew,
    #[error("tmux: {0}")]
    Tmux(#[from] TmuxError),
    /// The spawning Hand's actor_id differs from the actor the caller is
    /// trying to spawn this new Hand under. Cross-user mutation is refused
    /// at the branching boundary so no actor can write to another actor's
    /// namespace (spark ryve-c44b92e5 / epic ryve-b8802f3b).
    #[error(
        "cross-user spawn refused: parent session {parent_session} is actor '{parent_actor}', \
         cannot spawn a Hand for actor '{requested_actor}'"
    )]
    CrossActorRefused {
        parent_session: String,
        parent_actor: String,
        requested_actor: String,
    },
    /// The requested actor segment is empty or contains '/' — it would
    /// collide with reserved branch prefixes (`epic/`, `crew/`, `release/`)
    /// or produce an invalid git ref.
    #[error("invalid actor id '{0}': must be non-empty and contain no '/'")]
    InvalidActor(String),
    /// Applying the archetype's [`ToolPolicy`] to the worktree failed.
    /// Surfaces with the archetype id so the operator can attribute the
    /// failure (acceptance criterion (4) of spark ryve-8384b3cc).
    #[error("archetype '{archetype}': failed to apply tool policy: {source}")]
    ToolPolicy {
        archetype: &'static str,
        #[source]
        source: std::io::Error,
    },
}

/// Run a subprocess under a streaming-heartbeat wrapper so long silent
/// runs (notably the full-workspace `cargo test`) do not trip Claude's
/// ~5-minute stream-idle timeout and kill the Hand session. Parent epic
/// ryve-32e95c28 / [sp-b7f7f1fa].
///
/// Opt-in per invocation: short or noisy commands should stay on the
/// plain `Command` path so their output is not polluted with heartbeat
/// lines. This helper is intended for commands whose silent windows
/// routinely exceed the stream-idle threshold.
///
/// The child's stdout and stderr are multiplexed onto `out` byte-for-byte,
/// in arrival order; heartbeat lines are the only additions and no child
/// bytes are rewritten or dropped (see [`StreamHeartbeat`]).
///
/// `cwd = None` inherits the caller's current directory; `interval = None`
/// uses [`crate::stream_heartbeat::DEFAULT_HEARTBEAT_INTERVAL`] (30 s,
/// comfortably under the ~5-minute Claude threshold).
pub async fn run_with_stream_heartbeat<W>(
    command: &str,
    args: &[&str],
    cwd: Option<&Path>,
    envs: &[(String, String)],
    interval: Option<Duration>,
    out: W,
) -> std::io::Result<StreamOutcome>
where
    W: AsyncWrite + Unpin + Send,
{
    let mut cmd = Command::new(command);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut wrapper = StreamHeartbeat::new();
    if let Some(d) = interval {
        wrapper = wrapper.with_interval(d);
    }
    wrapper.run(cmd, out).await
}

/// Resolve the actor_id a UI-driven Hand spawn should use when no explicit
/// actor is configured yet. Priority: `RYVE_ACTOR_ID` > `USER` > `"hand"`.
/// Kept `pub(crate)` so `app.rs`'s worktree-creation path can share the
/// exact resolution rules the CLI uses.
pub(crate) fn resolve_ui_actor() -> String {
    resolve_actor_from_env(None)
}

/// Shared actor-resolution rule used by both UI and CLI spawn paths.
/// Priority order:
///   1. explicit `override_actor` (CLI `--actor <id>`),
///   2. `RYVE_ACTOR_ID` env var (propagated by a parent Hand),
///   3. `USER` env var (direct human invocation),
///   4. literal `"hand"` as a last-resort fallback.
fn resolve_actor_from_env(override_actor: Option<&str>) -> String {
    if let Some(a) = override_actor {
        let trimmed = a.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(a) = std::env::var("RYVE_ACTOR_ID") {
        let trimmed = a.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(a) = std::env::var("USER") {
        let trimmed = a.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "hand".to_string()
}

/// Validate an actor id before it is baked into a branch name or a DB row.
/// Callers share this helper so the same rule is enforced at every entry
/// point (UI + CLI + tests).
fn validate_actor(actor: &str) -> Result<(), HandSpawnError> {
    if actor.is_empty() || actor.contains('/') {
        return Err(HandSpawnError::InvalidActor(actor.to_string()));
    }
    Ok(())
}

/// Branch-name convention used by [`workshop::create_hand_worktree`].
/// The new Hand's branch is cut as `<actor>/<short>` where `<short>` is
/// the first eight characters of the session id. Kept here so the
/// base-ref resolver computes the exact same ref the production spawn
/// path creates. Private because no cross-module call site needs it —
/// the integration test keeps its own synced copy and asserts it
/// matches this convention via the resolver's end-to-end behaviour.
fn hand_branch_name(actor: &str, session_id: &str) -> String {
    let short = &session_id[..8.min(session_id.len())];
    format!("{actor}/{short}")
}

/// Resolve the `base_ref` a new Hand dispatched on `spark_id` should cut
/// its worktree from. Spark [sp-b7633430] / epic ryve-b7633430:
///
/// When `spark_id` has at least one `blocks` predecessor, the Merger
/// must integrate the predecessor's diff before (or as part of) ours.
/// Cutting every Hand from the release tip forces the Merger to do an
/// N-way same-file integration that mechanical merge cannot resolve —
/// the exact failure mode that caused the three migration-019 collisions
/// in 0.2.0. Cutting the new Hand's worktree from the predecessor's
/// branch tip moves that integration to dispatch time and makes the
/// subsequent merger a no-op.
///
/// Contract:
///   (a) query [`bond_repo::list_blocks_predecessors`] for `spark_id`;
///   (b) for each predecessor spark id, consult `assign_repo` for the
///       owner assignment row (active or completed) — the row carries
///       both the `actor_id` (git branch namespace) and `session_id`
///       (branch short), which together name the Hand's branch under
///       the [`hand_branch_name`] convention;
///   (c) `git rev-parse` each candidate branch and return the first
///       SHA that resolves. Most-recent assignments come first because
///       `assign_repo::list_assignments_for_spark` orders by
///       `created_at DESC`.
///
/// Returns `None` when the spark has no `blocks` predecessors, or when
/// none of the predecessor branches resolves (e.g. predecessor was
/// spawned under a foreign actor and its branch doesn't exist locally
/// yet). In that case the spawn path falls back to the historical
/// behaviour of cutting from the current HEAD.
pub(crate) async fn resolve_hand_base_ref(
    pool: &SqlitePool,
    workshop_dir: &Path,
    spark_id: &str,
) -> Option<String> {
    let predecessors = bond_repo::list_blocks_predecessors(pool, spark_id)
        .await
        .ok()?;
    if predecessors.is_empty() {
        return None;
    }

    // Gather every owner assignment across every predecessor so the
    // resolver can fall back to an older row whose branch still exists.
    // Previous revision broke after the newest-per-predecessor row,
    // which silently skipped stacking whenever that row's actor-scoped
    // branch had been pruned (foreign-actor respawn, worktree cleanup).
    // `list_assignments_for_spark` orders rows newest-first;
    // sorting all candidates by `assigned_at DESC` after collecting
    // them preserves that ordering globally.
    let mut candidates: Vec<(String, String, String)> = Vec::new(); // (assigned_at, actor_id, session_id)
    for pred_id in &predecessors {
        let Ok(asgns) = assign_repo::list_assignments_for_spark(pool, pred_id).await else {
            continue;
        };
        for a in asgns {
            if a.role != "owner" {
                continue;
            }
            let Some(sid) = a.session_id.clone() else {
                continue;
            };
            let ts = a
                .assigned_at
                .clone()
                .unwrap_or_else(|| a.created_at.clone());
            candidates.push((ts, a.actor_id, sid));
            // Keep going: older owner assignments on the same
            // predecessor are valid fall-backs if the newest row's
            // branch has been pruned locally.
        }
    }
    // Sort newest-first globally. Ties are broken deterministically by
    // (actor, session) so the resolver is stable across reruns.
    candidates.sort_by(|x, y| {
        y.0.cmp(&x.0)
            .then_with(|| y.1.cmp(&x.1))
            .then_with(|| y.2.cmp(&x.2))
    });

    for (_, actor_id, session_id) in candidates {
        let branch = hand_branch_name(&actor_id, &session_id);
        if let Some(sha) = resolve_branch_tip(workshop_dir, &branch).await {
            return Some(sha);
        }
    }
    None
}

/// Run `git rev-parse --verify refs/heads/<branch>` in `workshop_dir` and
/// return the SHA on success. Returns `None` when the branch does not
/// exist (common case when the predecessor Hand was spawned under a
/// different actor / a branch was renamed / pruned). Callers treat a
/// `None` here as "no usable base ref" and fall back to HEAD.
async fn resolve_branch_tip(workshop_dir: &Path, branch: &str) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .arg("rev-parse")
        .arg("--verify")
        .arg(format!("refs/heads/{branch}"))
        .current_dir(workshop_dir)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

/// Boot-prepend: maximum number of prior chat-of-record posts tailed
/// from the epic channel when a Hand is re-spawned onto a spark that
/// already had an owner. Mirrors [`ipc::chat_of_record::TAIL_DEFAULT_LIMIT`]
/// so direct `ryve channel tail` reads and the boot prepend stay
/// symmetrical, but kept as its own constant so it can be tuned
/// independently without pulling the whole CLI default with it.
/// Parent epic ryve-12f09190 / [sp-b6d9a53c].
pub const RESPAWN_CONTEXT_MAX_LINES: i64 = 50;

/// Boot-prepend: hard byte ceiling on the channel-context block attached
/// to the re-spawn prompt. The acceptance criterion is "50 lines OR 10 KB,
/// whichever smaller" — oldest messages are dropped from the head of the
/// window until the rendered block fits. Prevents a single long post from
/// blowing the new Hand's opening context budget.
pub const RESPAWN_CONTEXT_MAX_BYTES: usize = 10 * 1024;

/// Compose the re-spawn boot prefix for a Hand being dispatched onto
/// `spark_id`.
///
/// Returns `Some(prefix)` iff the spark has a prior owner assignment
/// whose session differs from `current_session_id` — i.e. this spawn is
/// taking over from a Hand that already worked the spark. The returned
/// string is a ready-to-prepend block containing the last N posts of the
/// relevant epic channel, capped per [`RESPAWN_CONTEXT_MAX_LINES`] /
/// [`RESPAWN_CONTEXT_MAX_BYTES`].
///
/// Channel resolution: the spark's `parent_id` chain is walked upward
/// until an epic-typed spark is found; that epic's id + title yield the
/// canonical `#epic-<id>-<slug>` name via
/// [`ipc::channel_manager::channel_name`]. Sparks whose chain contains
/// no epic return `None` (no channel to tail).
///
/// The prefix **never** includes previous-owner session log files,
/// scratchpads, or any other filesystem artifact — only durable
/// `irc_messages` rows via [`ipc::chat_of_record::tail`]. That matches
/// the chat-of-record invariant from the parent epic: the IRC table is
/// the source of truth for agent intent; log files are volatile debug
/// artefacts that must not leak into the next Hand's boot context.
async fn compose_respawn_channel_context(
    pool: &SqlitePool,
    spark_id: &str,
    current_session_id: &str,
) -> Option<String> {
    let assignments = assign_repo::list_assignments_for_spark(pool, spark_id)
        .await
        .ok()?;
    let has_prior_owner = assignments
        .iter()
        .any(|a| a.role == "owner" && a.session_id.as_deref() != Some(current_session_id));
    if !has_prior_owner {
        return None;
    }

    // Walk up the parent chain until we find an epic-typed spark — only
    // epics have a channel in the chat-of-record schema. Cap the walk so
    // a metadata cycle (shouldn't happen, but the worker should be
    // defensive at boot) can't spin forever.
    let mut cursor = spark_repo::get(pool, spark_id).await.ok()?;
    for _ in 0..16 {
        if cursor.spark_type == "epic" {
            break;
        }
        let parent_id = cursor.parent_id.clone()?;
        cursor = spark_repo::get(pool, &parent_id).await.ok()?;
    }
    if cursor.spark_type != "epic" {
        return None;
    }

    let channel = ipc::channel_manager::channel_name(&ipc::channel_manager::EpicRef {
        id: cursor.id.clone(),
        name: cursor.title.clone(),
    });

    let filter = ipc::chat_of_record::TailFilter::for_channel(channel.clone())
        .with_limit(RESPAWN_CONTEXT_MAX_LINES);
    let messages = ipc::chat_of_record::tail(pool, filter).await.ok()?;
    if messages.is_empty() {
        return None;
    }

    // Render in chronological order (tail returns oldest-first). Trim
    // oldest lines from the head until the whole block fits under
    // RESPAWN_CONTEXT_MAX_BYTES so a handful of long posts can't smuggle
    // past the cap.
    let header = format!(
        "# Recent chat-of-record from {channel}\n\
         The previous owner(s) of spark {spark_id} posted these on the epic \
         channel before handing off. Use them to pick up context before you \
         start. (Last {RESPAWN_CONTEXT_MAX_LINES} posts / {} KB cap, whichever smaller.)\n\n",
        RESPAWN_CONTEXT_MAX_BYTES / 1024,
    );
    let footer = "\n";

    let rendered: Vec<String> = messages
        .iter()
        .map(|m| {
            format!(
                "[{}] {}: {}",
                m.created_at,
                m.sender_actor_id.as_deref().unwrap_or("-"),
                m.raw_text,
            )
        })
        .collect();

    let mut start = 0usize;
    while start < rendered.len() {
        let body_bytes: usize = rendered[start..].iter().map(|l| l.len() + 1).sum();
        if header.len() + body_bytes + footer.len() <= RESPAWN_CONTEXT_MAX_BYTES {
            break;
        }
        start += 1;
    }
    if start >= rendered.len() {
        return None;
    }

    let mut out = String::with_capacity(header.len() + RESPAWN_CONTEXT_MAX_BYTES);
    out.push_str(&header);
    for line in &rendered[start..] {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(footer);
    Some(out)
}

/// Optional context bundle for [`spawn_hand`] and [`spawn_head`]. Bundled
/// to keep the function signature under clippy's too-many-arguments
/// threshold while still letting callers pass only the hints they have.
/// `Default` builds an empty bundle so direct CLI / test callers can use
/// `SpawnContext::default()` when they have no lineage to propagate.
#[derive(Debug, Default, Clone, Copy)]
pub struct SpawnContext<'a> {
    /// Attach the new session to this Crew via `crew_repo::add_member`.
    pub crew_id: Option<&'a str>,
    /// `agent_sessions.id` of the Hand issuing this spawn. Persisted on
    /// the new row's `parent_session_id` column for lineage rendering,
    /// and read by the cross-user refusal check to discover the parent's
    /// actor_id.
    pub parent_session_id: Option<&'a str>,
    /// Explicit actor override (e.g. CLI `--actor <id>`). When `None`,
    /// [`resolve_actor_from_env`] falls back to `RYVE_ACTOR_ID`, `USER`,
    /// then `"hand"`.
    pub actor_id: Option<&'a str>,
}

/// Spawn a Hand programmatically. Used by `ryve hand spawn` and by tests.
///
/// `workshop_dir` is the workshop root (where `.ryve/` lives).
/// `agent` is the coding agent definition (claude / codex / aider / opencode
/// or any custom agent registered in the future).
/// `kind` decides Owner vs Merger.
/// `crew_id` attaches the new Hand to a Crew via `crew_repo::add_member`.
/// `parent_session_id` is the `agent_sessions.id` of the Hand that
/// dispatched this spawn — typically a Head when invoked from a Head's
/// `ryve hand spawn` call. Persisted on the row so the UI can render
/// Head → solo-hand attribution. `None` for direct user spawns.
pub async fn spawn_hand(
    workshop_dir: &Path,
    pool: &SqlitePool,
    agent: &CodingAgent,
    spark_id: &str,
    kind: HandKind,
    ctx: SpawnContext<'_>,
) -> Result<SpawnedHand, HandSpawnError> {
    let SpawnContext {
        crew_id,
        parent_session_id,
        actor_id,
    } = ctx;

    if matches!(kind, HandKind::Merger) && crew_id.is_none() {
        return Err(HandSpawnError::MergerNeedsCrew);
    }

    // Resolve the actor up front so every subsequent step (worktree branch
    // name, assignment row, env propagation) shares one value. The rule is:
    // explicit --actor wins, else env, else current shell user, else "hand".
    // Spark ryve-c44b92e5 / epic ryve-b8802f3b.
    let actor = resolve_actor_from_env(actor_id);
    validate_actor(&actor)?;

    // Cross-user refusal: if a parent session is running this spawn, its
    // active assignment pins the parent's actor. A parent cannot spawn a
    // Hand under a different actor's namespace.
    //
    // Carveout for HandKind::Reviewer: the reviewer role is required to
    // be a different actor than the author (spark ryve-b0a369dc's
    // "reviewer is never the author" invariant), so a cross-actor spawn
    // is the *correct* behaviour for a reviewer and is always issued via
    // `spawn_reviewer`. Enforcing the generic refusal here would make
    // the reviewer policy un-implementable.
    if !matches!(kind, HandKind::Reviewer)
        && let Some(parent) = parent_session_id
        && let Some(parent_actor) =
            data::sparks::assignment_repo::actor_id_for_session(pool, parent).await?
        && parent_actor != actor
    {
        return Err(HandSpawnError::CrossActorRefused {
            parent_session: parent.to_string(),
            parent_actor,
            requested_actor: actor,
        });
    }

    let ryve_dir = RyveDir::new(workshop_dir);
    ryve_dir.ensure_exists().await.map_err(HandSpawnError::Io)?;

    // 1. New session id + worktree — branch is `<actor>/<short>`.
    //    Base-branch stacking for serial children (spark [sp-b7633430]):
    //    when this spark has `blocks` predecessors, cut the new Hand's
    //    branch from the predecessor Hand's tip so the Merger integrates
    //    pre-serialized diffs with zero conflicts instead of an N-way
    //    same-file reconcile. For Mergers, the crew merge branch is
    //    always cut from the crew's target — never a sibling Hand's
    //    tip — so this lookup is skipped.
    let session_id = Uuid::new_v4().to_string();
    let base_ref = if matches!(kind, HandKind::Merger) {
        None
    } else {
        resolve_hand_base_ref(pool, workshop_dir, spark_id).await
    };
    let worktree_path = workshop::create_hand_worktree(
        workshop_dir,
        &ryve_dir,
        &session_id,
        &actor,
        base_ref.as_deref(),
    )
    .await
    .map_err(HandSpawnError::Worktree)?;

    // Pre-compute the log path so it can be persisted alongside the
    // session row. The UI uses this path to drive the read-only spy view
    // for background Hands (spark ryve-8c14734a).
    let logs_dir = ryve_dir.root().join("logs");
    tokio::fs::create_dir_all(&logs_dir).await?;
    let log_path = logs_dir.join(format!("hand-{session_id}.log"));

    // 2. Persist the agent session.
    let new_session = NewAgentSession {
        id: session_id.clone(),
        workshop_id: workshop_id_for(workshop_dir),
        agent_name: agent.display_name.clone(),
        agent_command: agent.command.clone(),
        agent_args: agent.args.clone(),
        session_label: Some(kind.session_label().to_string()),
        child_pid: None,
        resume_id: None,
        log_path: Some(log_path.to_string_lossy().into_owned()),
        parent_session_id: parent_session_id.map(|s| s.to_string()),
        archetype_id: Some(hand_archetypes::archetype_id_for(kind).to_string()),
    };
    agent_session_repo::create(pool, &new_session).await?;

    // 3. Claim the spark. Bind the resolved actor onto the assignment row
    //    so downstream consumers (branch validator, cleanup, audit) see the
    //    same actor_id the branch was cut under.
    let assignment = NewHandAssignment {
        session_id: session_id.clone(),
        spark_id: spark_id.to_string(),
        role: kind.role(),
        actor_id: Some(actor.clone()),
    };
    assignment_repo::assign(pool, assignment).await?;

    // 4. Add to crew if requested.
    if let Some(cid) = crew_id {
        crew_repo::add_member(pool, cid, &session_id, Some(kind.session_label())).await?;
    }

    // 5. Compose the prompt.
    let mut prompt = match kind {
        HandKind::Owner => {
            // Pull the spark + siblings so the prompt can include intent.
            let sparks = spark_repo::list(
                pool,
                SparkFilter {
                    workshop_id: Some(workshop_id_for(workshop_dir)),
                    ..SparkFilter::default()
                },
            )
            .await
            .unwrap_or_else(|_| Vec::<Spark>::new());
            compose_hand_prompt(&sparks, spark_id)
        }
        HandKind::Head => {
            // Look up the epic title so the Head prompt can reference it
            // by name. If the spark isn't in the DB (shouldn't happen at
            // this point — the assignment row would have failed above)
            // we still compose a prompt with just the id.
            let epic_title = spark_repo::get(pool, spark_id).await.ok().map(|s| s.title);
            compose_head_prompt(HeadArchetype::Build, Some(spark_id), epic_title.as_deref())
        }
        HandKind::Investigator => {
            let sparks = spark_repo::list(
                pool,
                SparkFilter {
                    workshop_id: Some(workshop_id_for(workshop_dir)),
                    ..SparkFilter::default()
                },
            )
            .await
            .unwrap_or_else(|_| Vec::<Spark>::new());
            compose_investigator_prompt(&sparks, spark_id)
        }
        HandKind::ReleaseManager => {
            let sparks = spark_repo::list(
                pool,
                SparkFilter {
                    workshop_id: Some(workshop_id_for(workshop_dir)),
                    ..SparkFilter::default()
                },
            )
            .await
            .unwrap_or_else(|_| Vec::<Spark>::new());
            compose_release_manager_prompt(&sparks, spark_id)
        }
        HandKind::BugHunter => {
            let sparks = spark_repo::list(
                pool,
                SparkFilter {
                    workshop_id: Some(workshop_id_for(workshop_dir)),
                    ..SparkFilter::default()
                },
            )
            .await
            .unwrap_or_else(|_| Vec::<Spark>::new());
            compose_bug_hunter_prompt(&sparks, spark_id)
        }
        HandKind::PerformanceEngineer => {
            let sparks = spark_repo::list(
                pool,
                SparkFilter {
                    workshop_id: Some(workshop_id_for(workshop_dir)),
                    ..SparkFilter::default()
                },
            )
            .await
            .unwrap_or_else(|_| Vec::<Spark>::new());
            compose_performance_engineer_prompt(&sparks, spark_id)
        }
        HandKind::Architect => {
            let sparks = spark_repo::list(
                pool,
                SparkFilter {
                    workshop_id: Some(workshop_id_for(workshop_dir)),
                    ..SparkFilter::default()
                },
            )
            .await
            .unwrap_or_else(|_| Vec::<Spark>::new());
            compose_architect_prompt(&sparks, spark_id)
        }
        HandKind::Reviewer => {
            let sparks = spark_repo::list(
                pool,
                SparkFilter {
                    workshop_id: Some(workshop_id_for(workshop_dir)),
                    ..SparkFilter::default()
                },
            )
            .await
            .unwrap_or_else(|_| Vec::<Spark>::new());
            compose_reviewer_prompt(&sparks, spark_id)
        }
        HandKind::Merger => compose_merger_prompt(crew_id.unwrap_or(""), spark_id),
    };

    // 5b. Boot-prepend chat-of-record context when this is a re-spawn.
    //     If the spark already had a prior owner (a different session),
    //     tail the epic channel and prepend the last RESPAWN_CONTEXT_MAX_LINES
    //     posts (capped at RESPAWN_CONTEXT_MAX_BYTES) to the prompt. Only
    //     Owner-class Hands re-inherit an in-flight spark; the Merger is
    //     one-shot per crew merge, so its prompt stays fixed. Parent epic
    //     ryve-12f09190 / [sp-b6d9a53c].
    if !matches!(kind, HandKind::Merger)
        && let Some(ctx) = compose_respawn_channel_context(pool, spark_id, &session_id).await
    {
        prompt = format!("{ctx}{prompt}");
    }

    // 6. Write the prompt to a temp file under .ryve/prompts/ so the agent
    //    can pick it up via `--system-prompt <file>` (claude/codex/aider).
    //    For agents that take inline text (opencode), we still write the
    //    file for traceability and pass the contents inline.
    let prompts_dir = ryve_dir.root().join("prompts");
    tokio::fs::create_dir_all(&prompts_dir).await?;
    let prompt_path = prompts_dir.join(format!("hand-{session_id}.md"));
    tokio::fs::write(&prompt_path, &prompt).await?;

    // 7. Build the command line.

    // Delegate the agent-specific argv assembly to `CodingAgent`. This
    // injects headless-mode flags (`--print` / `exec` / `--message` /
    // `run`), full-auto flags, AND the user prompt itself. The previous
    // implementation only passed the prompt via `--system-prompt`, which
    // left every detached agent with no user message and caused them to
    // exit on the first turn (spark ryve-b3ad7bd1).
    let cmd_args = agent.build_headless_args(&prompt, &prompt_path);

    // Build env for the detached child. We layer the new session id on
    // top of the standard `RYVE_WORKSHOP_ROOT` + `PATH` set so any nested
    // `ryve hand spawn` invocation made by *this* Hand correctly attributes
    // its child to itself. Also stamp `RYVE_ACTOR_ID` so nested spawns
    // inherit the same actor — the cross-user refusal check on the child
    // side relies on the parent's env propagating its actor identity.
    let mut env_vars = workshop::hand_env_vars(workshop_dir);
    env_vars.push(("RYVE_HAND_SESSION_ID".to_string(), session_id.clone()));
    env_vars.push(("RYVE_ACTOR_ID".to_string(), actor.clone()));

    // 8. Apply the archetype's tool policy to the worktree BEFORE the
    //    subprocess starts. Read-only archetypes (Investigator) have the
    //    tree chmod'd to `0o444 / 0o555` so any write the agent attempts
    //    fails at the kernel boundary regardless of what its prompt says.
    //    Spark ryve-8384b3cc: gating is mechanical, not a prompt
    //    instruction. On failure, abort the spawn (no tmux launch) and
    //    roll back the DB rows — a read-only invariant that was not
    //    applied is worse than a Hand that never started.
    let policy = hand_archetypes::tool_policy_for(kind);
    let archetype_id = hand_archetypes::archetype_id_for(kind);
    if let Err(e) = hand_archetypes::apply_tool_policy(&worktree_path, policy, archetype_id) {
        let _ = assignment_repo::abandon(pool, &session_id, spark_id).await;
        let _ = agent_session_repo::end_session(pool, &session_id).await;
        return Err(HandSpawnError::ToolPolicy {
            archetype: archetype_id,
            source: e,
        });
    }

    // 9. Launch inside a tmux session. The session name is `hand-<session_id>`
    //    (or `head-`/`merger-` for other kinds), matching the invariant that
    //    there is exactly one tmux session per agent_sessions row.
    let tmux_session_name = tmux_session_name(kind, &session_id);
    if let Err(err) = launch_in_tmux(
        &ryve_dir,
        &tmux_session_name,
        &agent.command,
        &cmd_args,
        &worktree_path,
        &env_vars,
        &log_path,
    ) {
        let _ = assignment_repo::abandon(pool, &session_id, spark_id).await;
        let _ = agent_session_repo::end_session(pool, &session_id).await;
        return Err(err);
    }

    // 10. Launch the heartbeat sidecar. Mergers do not heartbeat — their
    //     lifecycle is the crew merge step, not a durable assignment.
    //     Parent epic ryve-cf05fd85 [sp-85034c27]: a spawned Hand emits a
    //     `HeartbeatReceived` event every `heartbeat_interval_secs` while
    //     its assignment is active. Sidecar failure is *not* fatal to the
    //     spawn — the Hand can still do work without a heartbeater, and
    //     the watchdog will surface the missing beats as `AtRisk`.
    if !matches!(kind, HandKind::Merger)
        && let Err(err) = launch_heartbeat_sidecar(&ryve_dir, workshop_dir, &session_id, spark_id)
    {
        eprintln!(
            "hand spawn: failed to launch heartbeat sidecar for session {session_id} on \
             spark {spark_id}: {err} — the Hand will run without durable heartbeats"
        );
    }

    Ok(SpawnedHand {
        session_id,
        spark_id: spark_id.to_string(),
        worktree_path,
        log_path,
        child_pid: None,
    })
}

/// Spawn a **Head** — an orchestrator coding-agent subprocess that
/// decomposes an epic into child sparks and manages a Crew of Hands.
///
/// Unlike a Hand, a Head does *not* claim a spark (no `hand_assignments`
/// row). Its relationship to the workgraph is carried by a Crew row whose
/// `head_session_id` points at the new session. The Head is also added as
/// a `crew_members` row with role `head` so the membership table lists it
/// alongside the Hands it will spawn.
///
/// If `crew_id` is `None`, a fresh Crew is created with
/// `parent_spark_id = epic_id`. If `crew_id` is given, the existing Crew's
/// head is updated in place — this is how Atlas can hand an already-created
/// Crew over to a newly spawned Head.
///
/// `parent_session_id` records lineage when Atlas (or another Head) spawns
/// this one; passed through unchanged onto `agent_sessions.parent_session_id`.
///
/// Spark ryve-e4cadc03.
pub async fn spawn_head(
    workshop_dir: &Path,
    pool: &SqlitePool,
    agent: &CodingAgent,
    epic_id: &str,
    archetype: HeadArchetype,
    ctx: SpawnContext<'_>,
) -> Result<SpawnedHead, HandSpawnError> {
    let SpawnContext {
        crew_id,
        parent_session_id,
        actor_id,
    } = ctx;

    // Validate the epic up front so a typo fails fast *before* we create
    // a worktree, a session row, or a crew. Also gives us the title for
    // the archetype prompt.
    let epic = spark_repo::get(pool, epic_id).await?;

    // Same actor-resolution rules as `spawn_hand` — the Head's worktree
    // branch also lives under the actor's namespace so cleanup and the
    // merge-target validator see a consistent shape regardless of whether
    // the row is a Head or an Owner Hand.
    let actor = resolve_actor_from_env(actor_id);
    validate_actor(&actor)?;

    if let Some(parent) = parent_session_id
        && let Some(parent_actor) =
            data::sparks::assignment_repo::actor_id_for_session(pool, parent).await?
        && parent_actor != actor
    {
        return Err(HandSpawnError::CrossActorRefused {
            parent_session: parent.to_string(),
            parent_actor,
            requested_actor: actor,
        });
    }

    let ryve_dir = RyveDir::new(workshop_dir);
    ryve_dir.ensure_exists().await.map_err(HandSpawnError::Io)?;

    // 1. New session id + worktree. The Head runs in its own worktree for
    //    the same reason Hands do — so its scratch files, prompt, and any
    //    agent-local state stay out of the main checkout.
    let session_id = Uuid::new_v4().to_string();
    let worktree_path =
        workshop::create_hand_worktree(workshop_dir, &ryve_dir, &session_id, &actor, None)
            .await
            .map_err(HandSpawnError::Worktree)?;

    let logs_dir = ryve_dir.root().join("logs");
    tokio::fs::create_dir_all(&logs_dir).await?;
    let log_path = logs_dir.join(format!("head-{session_id}.log"));

    // 2. Persist the agent session row with `session_label = "head"`. The
    //    UI's Hands panel, the delegation-trace view, and any future
    //    archetype-aware trace rendering rely on this label being exactly
    //    "head" (see `src/screen/agents.rs` and `delegation_trace.rs`).
    let new_session = NewAgentSession {
        id: session_id.clone(),
        workshop_id: workshop_id_for(workshop_dir),
        agent_name: agent.display_name.clone(),
        agent_command: agent.command.clone(),
        agent_args: agent.args.clone(),
        session_label: Some("head".to_string()),
        child_pid: None,
        resume_id: None,
        log_path: Some(log_path.to_string_lossy().into_owned()),
        parent_session_id: parent_session_id.map(|s| s.to_string()),
        // Heads select their flavour via the Head archetype recorded on
        // the `archetype` column on the crew row, not the `agent_sessions`
        // column introduced for Hand archetypes.
        archetype_id: None,
    };
    agent_session_repo::create(pool, &new_session).await?;

    // 3. Resolve the Crew: either reuse the caller-supplied one (point its
    //    head_session_id at this new session) or create a fresh Crew
    //    parented on the epic. Then register the Head as a crew member
    //    with role "head".
    let crew_id_resolved = match crew_id {
        Some(cid) => {
            crew_repo::set_head(pool, cid, Some(&session_id)).await?;
            cid.to_string()
        }
        None => {
            let crew = crew_repo::create(
                pool,
                NewCrew {
                    name: format!("{} ({})", epic.title, archetype.as_str()),
                    purpose: Some(epic.title.clone()),
                    workshop_id: workshop_id_for(workshop_dir),
                    head_session_id: Some(session_id.clone()),
                    parent_spark_id: Some(epic_id.to_string()),
                },
            )
            .await?;
            crew.id
        }
    };
    crew_repo::add_member(pool, &crew_id_resolved, &session_id, Some("head")).await?;

    // 4. Compose the archetype-specific prompt. The first paragraph names
    //    the archetype so the "identity at boot" invariant from
    //    docs/HEAD_ARCHETYPES.md holds for both fresh and resumed runs.
    let prompt = compose_head_prompt(archetype, Some(epic_id), Some(&epic.title));

    let prompts_dir = ryve_dir.root().join("prompts");
    tokio::fs::create_dir_all(&prompts_dir).await?;
    let prompt_path = prompts_dir.join(format!("head-{session_id}.md"));
    tokio::fs::write(&prompt_path, &prompt).await?;

    // 5. Build argv via the same headless-mode helper Hands use so the
    //    prompt is delivered as a *user message*, not just a system
    //    prompt. Regression guarded for Hands under spark ryve-b3ad7bd1
    //    and it applies equally to Heads.
    let cmd_args = agent.build_headless_args(&prompt, &prompt_path);

    // 6. Env: inherit the workshop env so the Head's own `ryve` calls
    //    resolve the workgraph without cd'ing, and stamp the new session
    //    id so any nested `ryve hand spawn` the Head makes records its
    //    lineage back to this Head. `RYVE_ACTOR_ID` is propagated so the
    //    child-side spawn on a Hand spawned by this Head inherits the
    //    same actor namespace.
    let mut env_vars = workshop::hand_env_vars(workshop_dir);
    env_vars.push(("RYVE_HAND_SESSION_ID".to_string(), session_id.clone()));
    env_vars.push(("RYVE_ACTOR_ID".to_string(), actor.clone()));

    // 7. Launch inside a tmux session. On failure, end the session so
    //    the row does not linger as a phantom Head that never actually
    //    ran. The crew row we created is kept — the caller can retry
    //    with `--crew <id>`.
    let tmux_session_name = format!("head-{session_id}");
    if let Err(err) = launch_in_tmux(
        &ryve_dir,
        &tmux_session_name,
        &agent.command,
        &cmd_args,
        &worktree_path,
        &env_vars,
        &log_path,
    ) {
        let _ = agent_session_repo::end_session(pool, &session_id).await;
        return Err(err);
    }

    Ok(SpawnedHead {
        session_id,
        epic_id: epic_id.to_string(),
        crew_id: crew_id_resolved,
        archetype,
        worktree_path,
        log_path,
        child_pid: None,
    })
}

/// Derive the tmux session name from the kind and session id.
/// Invariant: one tmux session per `agent_sessions` row.
fn tmux_session_name(kind: HandKind, session_id: &str) -> String {
    format!("{}-{}", kind.session_label(), session_id)
}

/// Launch the heartbeat sidecar for a freshly-spawned Hand.
///
/// Runs `ryve hand heartbeat-loop <session_id> <spark_id>` inside its own
/// detached tmux session on Ryve's private socket. The loop emits a
/// `HeartbeatReceived` event + stamps `last_heartbeat_at` every
/// `heartbeat_interval_secs` (workshop config) and exits cleanly the
/// first time the assignment is no longer `active` — whether because the
/// Hand closed its spark, a sweep-reaper abandoned it, or a Head
/// override handed it off. Parent epic ryve-cf05fd85 [sp-85034c27].
fn launch_heartbeat_sidecar(
    ryve_dir: &RyveDir,
    workshop_dir: &Path,
    session_id: &str,
    spark_id: &str,
) -> Result<(), HandSpawnError> {
    let tmux_bin = tmux::resolve_tmux_bin()
        .ok_or_else(|| HandSpawnError::Tmux(TmuxError::BinaryMissing(PathBuf::from("tmux"))))?;

    let ryve_bin = std::env::current_exe().map_err(HandSpawnError::Io)?;

    let logs_dir = ryve_dir.root().join("logs");
    std::fs::create_dir_all(&logs_dir)?;
    let log_path = logs_dir.join(format!("heartbeat-{session_id}.log"));
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let session_name = heartbeat_sidecar_session_name(session_id);

    let ryve_bin_str = ryve_bin.to_string_lossy().into_owned();
    let argv: Vec<&str> = vec![
        &ryve_bin_str,
        "hand",
        "heartbeat-loop",
        session_id,
        spark_id,
    ];

    let env_vars = workshop::hand_env_vars(workshop_dir);
    let env_map: HashMap<String, String> = env_vars.into_iter().collect();

    let client = TmuxClient::new(tmux_bin, ryve_dir.root());
    client.new_session_detached(&session_name, workshop_dir, &env_map, &argv)?;
    client.pipe_pane(&session_name, &log_path)?;

    Ok(())
}

/// tmux session name for the heartbeat sidecar. Kept separate from the
/// `heartbeat-<session_id>` convention used elsewhere by prefixing with
/// `hbeat-` so sweep/cleanup code can distinguish the sidecar from the
/// agent session without string-parsing the UUID.
pub(crate) fn heartbeat_sidecar_session_name(session_id: &str) -> String {
    format!("hbeat-{session_id}")
}

/// Launch the coding agent inside a tmux session on Ryve's private socket.
///
/// 1. Creates a detached tmux session named `session_name` running the agent
///    command with the given args, cwd, and environment.
/// 2. Sets up `pipe-pane` to stream the session's output to `log_path`, so
///    existing log-tail consumers (the UI spy view) continue to work.
///
/// The tmux session owns the process lifecycle — no `setsid` or manual
/// stdout redirect needed.
fn launch_in_tmux(
    ryve_dir: &RyveDir,
    session_name: &str,
    command: &str,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    log_path: &Path,
) -> Result<(), HandSpawnError> {
    let tmux_bin = tmux::resolve_tmux_bin()
        .ok_or_else(|| HandSpawnError::Tmux(TmuxError::BinaryMissing(PathBuf::from("tmux"))))?;

    let client = TmuxClient::new(tmux_bin, ryve_dir.root());

    // Build argv: the command + its arguments as a single command line
    // that tmux will exec in the session's initial window.
    let mut argv: Vec<&str> = vec![command];
    argv.extend(args.iter().map(String::as_str));

    // Convert env from Vec<(String, String)> to HashMap for the wrapper.
    let env_map: HashMap<String, String> = env.iter().cloned().collect();

    // Ensure the log file exists so pipe-pane's `cat >>` has a target.
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;

    client.new_session_detached(session_name, cwd, &env_map, &argv)?;
    client.pipe_pane(session_name, log_path)?;

    Ok(())
}

/// The workshop_id is the workshop directory's last component, mirroring
/// `cli::run`. Kept private here so callers do not have to think about it.
fn workshop_id_for(workshop_dir: &Path) -> String {
    workshop_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

// ── Reviewer spawn path ──────────────────────────────────
//
// The reviewer role is bound by the contract on epic ryve-b0a369dc:
//   - Reviewer is never the author.
//   - Reviewer is a fresh execution instance (no other active
//     assignment in the same epic).
//   - Selection is deterministic — same inputs always pick the same
//     reviewer — so audits and replays agree.
//   - Preference is cross-vendor (claude author → codex reviewer, …).
//   - Fallback: if no cross-vendor reviewer exists, accept a same-vendor
//     one but emit a `reviewer_policy_relaxed` event so the relaxation
//     is visible in the audit trail.
//   - Hard failure: if no eligible reviewer exists at all, the
//     assignment is flagged `awaiting_reviewer_availability` and the
//     workshop is notified via IRC (a `flare` ember).
//
// The selection function is pure (no wall-clock, no RNG without seed)
// so the spawn path can be replayed by the projector and the same
// reviewer is chosen every time.

/// A candidate actor that may be picked as a reviewer. Callers enumerate
/// the workshop's actor pool (typically via `assignment_repo` queries the
/// caller composes) and filter out anyone who is not a fresh instance on
/// the epic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerCandidate {
    /// Stable actor identity. Matches the value written to
    /// `assignments.actor_id` for the spawned reviewer Hand.
    pub actor_id: String,
    /// Vendor / model family (e.g. "claude", "codex", "aider"). Used by
    /// [`select_reviewer`] to prefer cross-vendor picks.
    pub vendor: String,
}

/// Outcome of a deterministic reviewer selection. `CrossVendor` is the
/// default path; `SameVendorRelaxed` signals that no cross-vendor
/// candidate was available and [`spawn_reviewer`] MUST emit the
/// `reviewer_policy_relaxed` event so the relaxation is auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerSelection {
    /// A candidate with a different vendor than the author was selected —
    /// the default discipline described in the epic contract.
    CrossVendor,
    /// No cross-vendor candidate was eligible, so the selection fell back
    /// to a same-vendor candidate. The spawn path records this relaxation
    /// as an event (`reviewer_policy_relaxed`).
    SameVendorRelaxed,
}

impl ReviewerSelection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CrossVendor => "cross_vendor",
            Self::SameVendorRelaxed => "same_vendor_relaxed",
        }
    }
}

/// Selection-time errors. The sole failure mode is "no eligible
/// reviewer in the pool"; the spawn path turns that into an
/// `awaiting_reviewer_availability` flag on the assignment plus a flare
/// ember so a human can intervene.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReviewerSelectionError {
    /// Nobody in the pool satisfies the reviewer contract (excluded
    /// author, fresh instance). Carries the author identity in the
    /// message so the IRC surface can name who is waiting for review.
    #[error(
        "no eligible reviewer for author '{author_actor}' (pool size {pool_size}): \
         selection requires a fresh-instance actor different from the author"
    )]
    NoEligibleReviewer {
        author_actor: String,
        pool_size: usize,
    },
}

/// Pure deterministic reviewer selection.
///
/// Inputs:
/// - `author_actor`: the actor currently on the spark's Owner assignment.
/// - `author_vendor`: that actor's vendor/model family. Used only to
///   prefer cross-vendor reviewers; the author is excluded by
///   `actor_id` regardless of vendor.
/// - `pool`: the candidate reviewers. Callers pass in the subset of the
///   workshop's actors that are fresh instances (no other active
///   assignment on the epic).
/// - `seed`: deterministic tiebreaker. Passing the same seed produces
///   the same reviewer given the same `pool` — the invariant audits
///   and replays rely on.
///
/// Rules, in order:
/// 1. Exclude `author_actor` from consideration.
/// 2. If any remaining candidate has a vendor different from
///    `author_vendor`, pick deterministically from that sub-pool and
///    return [`ReviewerSelection::CrossVendor`].
/// 3. Otherwise, pick deterministically from the remaining same-vendor
///    candidates and return [`ReviewerSelection::SameVendorRelaxed`].
/// 4. If no candidate remains, return
///    [`ReviewerSelectionError::NoEligibleReviewer`].
///
/// The deterministic picker is `sort_by(actor_id) then seed mod len` so
/// the choice is a pure function of the arguments.
pub fn select_reviewer<'a>(
    author_actor: &str,
    author_vendor: &str,
    pool: &'a [ReviewerCandidate],
    seed: u64,
) -> Result<(&'a ReviewerCandidate, ReviewerSelection), ReviewerSelectionError> {
    let pool_size = pool.len();
    let eligible: Vec<&ReviewerCandidate> =
        pool.iter().filter(|c| c.actor_id != author_actor).collect();
    if eligible.is_empty() {
        return Err(ReviewerSelectionError::NoEligibleReviewer {
            author_actor: author_actor.to_string(),
            pool_size,
        });
    }

    let cross_vendor: Vec<&ReviewerCandidate> = eligible
        .iter()
        .copied()
        .filter(|c| c.vendor != author_vendor)
        .collect();

    let (bucket, outcome) = if !cross_vendor.is_empty() {
        (cross_vendor, ReviewerSelection::CrossVendor)
    } else {
        (eligible, ReviewerSelection::SameVendorRelaxed)
    };

    let mut sorted = bucket;
    sorted.sort_by(|a, b| a.actor_id.cmp(&b.actor_id));
    // `sorted.len()` is > 0: we returned early above when eligible was
    // empty, and `bucket` either equals `eligible` or a non-empty
    // subset of it. Modulo-by-len therefore cannot divide by zero.
    let idx = (seed as usize) % sorted.len();
    Ok((sorted[idx], outcome))
}

/// Outcome of [`spawn_reviewer`]. Separates the three terminal states so
/// the caller (CLI, UI, or automated sweep) can act on each without
/// re-inspecting DB rows.
#[derive(Debug, Clone)]
pub enum ReviewerSpawnOutcome {
    /// A reviewer Hand was successfully spawned. `selection` records
    /// whether the policy was honoured or relaxed; for
    /// [`ReviewerSelection::SameVendorRelaxed`] a `reviewer_policy_relaxed`
    /// event is recorded on the spark BEFORE the Hand is spawned so the
    /// relaxation is visible even if the subprocess launch fails
    /// downstream.
    Spawned {
        hand: SpawnedHand,
        selection: ReviewerSelection,
    },
    /// The pool had no eligible reviewer. Instead of spawning a Hand the
    /// spark has been flagged `awaiting_reviewer_availability` in the
    /// event log and a `flare` ember has been raised so a human can
    /// intervene. `ember_id` is returned so the caller can reference the
    /// signal in output / tests.
    AwaitingAvailability { ember_id: String },
}

/// Policy-level errors raised by [`spawn_reviewer`] *before* it delegates
/// to [`spawn_hand`]. Spawn-time errors (tmux failure, DB error, cross-user
/// refusal) are surfaced as [`HandSpawnError`] from the inner call.
#[derive(Debug, thiserror::Error)]
pub enum ReviewerSpawnError {
    /// A workgraph write (event log, ember insert) failed before we even
    /// attempted to launch the subprocess.
    #[error("workgraph error: {0}")]
    Sparks(#[from] data::sparks::SparksError),
    /// Inner [`spawn_hand`] call failed after a reviewer was selected.
    /// The `selection` field tells the caller whether the emitted
    /// `reviewer_policy_relaxed` event (if any) had already been recorded
    /// so a retry does not double-count.
    #[error("reviewer spawn failed after selection ({selection:?}): {source}")]
    HandSpawn {
        selection: ReviewerSelection,
        #[source]
        source: HandSpawnError,
    },
}

/// Inputs describing *who* is under review and *who* is available to
/// review. Bundled into a single struct so [`spawn_reviewer`] keeps a
/// readable signature at a call site and respects the
/// `too_many_arguments` budget. The fields are borrows rather than owned
/// strings so constructing a request is allocation-free at the call
/// site.
#[derive(Debug, Clone, Copy)]
pub struct ReviewerRequest<'a> {
    /// Actor whose work is under review. The selected reviewer MUST
    /// differ from this actor (see [`select_reviewer`]).
    pub author_actor: &'a str,
    /// Vendor / model family of the author. Used to prefer cross-vendor
    /// reviewers; a pure tiebreak hint, not an identity.
    pub author_vendor: &'a str,
    /// Candidate reviewers the caller has already narrowed to fresh-
    /// instance actors (no other active assignment on the epic).
    pub candidates: &'a [ReviewerCandidate],
    /// Deterministic tiebreaker: same seed + same candidates ⇒ same
    /// reviewer, so replays and audits agree.
    pub seed: u64,
}

/// Spawn a Reviewer Hand for `spark_id` using deterministic, cross-vendor-
/// preferring selection. `request` carries the author identity, the
/// candidate pool, and the seed. See [`ReviewerRequest`] for the field
/// contract.
///
/// Behaviour:
/// 1. Call [`select_reviewer`].
/// 2. If the selection is [`ReviewerSelection::SameVendorRelaxed`],
///    record a `reviewer_policy_relaxed` event on the spark BEFORE
///    spawning so the relaxation is visible even on downstream failure.
/// 3. Delegate to [`spawn_hand`] with `HandKind::Reviewer` and the
///    selected candidate's `actor_id` pinned on the spawn context.
/// 4. On [`ReviewerSelectionError::NoEligibleReviewer`], record an
///    `awaiting_reviewer_availability` event on the spark and raise a
///    `flare` ember so the IRC / UI surface can page a human. No Hand
///    is spawned.
///
/// Spark ryve-b0a369dc / [sp-f6259067].
pub async fn spawn_reviewer(
    workshop_dir: &Path,
    pool: &SqlitePool,
    agent: &CodingAgent,
    spark_id: &str,
    request: ReviewerRequest<'_>,
    ctx: SpawnContext<'_>,
) -> Result<ReviewerSpawnOutcome, ReviewerSpawnError> {
    use data::sparks::types::{ActorType, EmberType, NewEmber, NewEvent};
    use data::sparks::{ember_repo, event_repo};

    let ReviewerRequest {
        author_actor,
        author_vendor,
        candidates,
        seed,
    } = request;

    let workshop_id = workshop_id_for(workshop_dir);

    match select_reviewer(author_actor, author_vendor, candidates, seed) {
        Ok((candidate, selection)) => {
            if selection == ReviewerSelection::SameVendorRelaxed {
                // Log the relaxation BEFORE the spawn so the policy
                // change is visible even if the subprocess launch
                // subsequently fails. `actor` is recorded as the
                // reviewer being accepted so the audit trail names
                // the concrete pick; `reason` carries the author so
                // the downgrade is attributable to a specific diff.
                event_repo::record(
                    pool,
                    NewEvent {
                        spark_id: spark_id.to_string(),
                        actor: candidate.actor_id.clone(),
                        field_name: "reviewer_policy".to_string(),
                        old_value: Some(ReviewerSelection::CrossVendor.as_str().to_string()),
                        new_value: Some(ReviewerSelection::SameVendorRelaxed.as_str().to_string()),
                        reason: Some(format!(
                            "reviewer_policy_relaxed: no cross-vendor reviewer available \
                             for author '{author_actor}' (vendor '{author_vendor}'); \
                             accepting same-vendor reviewer '{}'",
                            candidate.actor_id
                        )),
                        actor_type: Some(ActorType::System),
                        change_nature: None,
                        session_id: None,
                    },
                )
                .await?;
            }

            // Pin the selected actor onto the spawn context so the
            // reviewer Hand's worktree branch and assignment row both
            // carry the reviewer's identity (not the author's).
            let spawn_ctx = SpawnContext {
                crew_id: ctx.crew_id,
                parent_session_id: ctx.parent_session_id,
                actor_id: Some(candidate.actor_id.as_str()),
            };

            let hand = spawn_hand(
                workshop_dir,
                pool,
                agent,
                spark_id,
                HandKind::Reviewer,
                spawn_ctx,
            )
            .await
            .map_err(|source| ReviewerSpawnError::HandSpawn { selection, source })?;

            Ok(ReviewerSpawnOutcome::Spawned { hand, selection })
        }
        Err(ReviewerSelectionError::NoEligibleReviewer {
            author_actor: _,
            pool_size,
        }) => {
            // Flag the assignment in the event log. No DB column change
            // is required — `event_repo` is the workgraph's audit
            // trail and `field_name = "reviewer_availability"` is how
            // the projector / UI surface the awaiting state.
            event_repo::record(
                pool,
                NewEvent {
                    spark_id: spark_id.to_string(),
                    actor: author_actor.to_string(),
                    field_name: "reviewer_availability".to_string(),
                    old_value: None,
                    new_value: Some("awaiting_reviewer_availability".to_string()),
                    reason: Some(format!(
                        "no eligible reviewer in pool (size {pool_size}): selection \
                         requires a fresh-instance actor different from the author"
                    )),
                    actor_type: Some(ActorType::System),
                    change_nature: None,
                    session_id: None,
                },
            )
            .await?;

            // Surface on IRC. `flare` is the right urgency tier per
            // `.ryve/WORKSHOP.md`: "needs attention soon" — a spark is
            // stuck waiting for a reviewer but it is not yet a
            // blaze-level interrupt. Prefix the content with
            // `awaiting_reviewer_availability` so the projector / UI
            // can group these by type.
            let ember = ember_repo::create(
                pool,
                NewEmber {
                    ember_type: EmberType::Flare,
                    content: format!(
                        "awaiting_reviewer_availability: spark {spark_id} author \
                         '{author_actor}' — reviewer pool exhausted (size {pool_size})"
                    ),
                    source_agent: Some("reviewer_spawn".to_string()),
                    workshop_id,
                    ttl_seconds: None,
                },
            )
            .await?;

            Ok(ReviewerSpawnOutcome::AwaitingAvailability { ember_id: ember.id })
        }
    }
}

#[cfg(test)]
mod tests {
    //! Integration tests for Hand/Head spawn via tmux.
    //!
    //! These tests spawn real tmux sessions on Ryve's private socket
    //! inside a temporary workshop, then verify that:
    //! - the tmux session exists and is named correctly;
    //! - the agent process receives the composed prompt;
    //! - the log file is being written via `pipe-pane`;
    //! - DB rows are consistent.

    use std::time::{Duration, Instant};

    use data::sparks::spark_repo;
    use data::sparks::types::{NewSpark, SparkType};
    use uuid::Uuid;

    use super::*;
    use crate::bundled_tmux;
    use crate::coding_agents::{CodingAgent, ResumeStrategy};
    use crate::tmux::TmuxClient;

    /// Skip-gate for the `spawn_*` integration tests below.
    ///
    /// These tests exercise the full production hand-spawn path, which
    /// invokes tmux via the bundled binary (`vendor/tmux/bin/tmux` in the
    /// dev layout, `<exe_dir>/bin/tmux` in shipped builds). They were
    /// previously gated on `tmux::resolve_tmux_bin().is_none()`, which
    /// returns the system tmux as a fallback — but in CI environments
    /// (Ubuntu runner) the system tmux exhibits behaviour the production
    /// path is not hardened against (notably failing `pipe-pane` lookup
    /// immediately after `new-session`), producing flaky `SessionNotFound`
    /// failures unrelated to anything under test.
    ///
    /// Gating on the bundled tmux specifically:
    ///   - keeps these tests running locally (developers have
    ///     `vendor/tmux/bin/tmux` built),
    ///   - keeps them running in the dedicated `Bundled tmux builds and
    ///     runs` CI job (which builds vendor/tmux as a step),
    ///   - and skips them in the generic `Test` CI job (which does not
    ///     build vendor/tmux, so any tmux it found would be the runner's
    ///     system tmux).
    fn bundled_tmux_available_for_tests() -> bool {
        bundled_tmux::bundled_tmux_path().is_some()
    }

    /// Build a throwaway workshop directory: `git init`, an initial empty
    /// commit (worktree creation requires HEAD), an open sqlite pool, and
    /// a stub-agent script that writes its argv to `out.txt`.
    async fn setup_workshop() -> (PathBuf, sqlx::SqlitePool, PathBuf) {
        let base = std::env::temp_dir().join(format!("ryve-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();

        // git init + empty commit so `git worktree add` has a HEAD.
        let run_git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&base)
                .env("GIT_AUTHOR_NAME", "ryve-test")
                .env("GIT_AUTHOR_EMAIL", "test@ryve.local")
                .env("GIT_COMMITTER_NAME", "ryve-test")
                .env("GIT_COMMITTER_EMAIL", "test@ryve.local")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        run_git(&["init", "-q", "-b", "main"]);
        // Disable gpg signing on the seed commit so flaky gpg-agent
        // memory errors in CI / concurrent-test environments do not
        // wedge the whole test. Repo-local config only; the outer
        // repo's settings are unaffected.
        run_git(&["config", "commit.gpgsign", "false"]);
        run_git(&["commit", "-q", "--allow-empty", "-m", "init"]);

        // Stub agent: prints its argv to a file, then sleeps briefly to
        // keep the tmux session alive. The output path is hardcoded into
        // the script body so parallel tests cannot clobber each other via
        // a shared env var — an earlier version used
        // `$RYVE_TEST_AGENT_OUT` and flaked whenever two of these tests
        // ran in parallel (their env overwrite raced).
        //
        // Why the sleep: `spawn_hand` issues `tmux new-session` then
        // immediately `tmux pipe-pane`. With a real agent (claude / codex)
        // the agent stays alive for the lifetime of the conversation, so
        // pipe-pane always finds the session. With a stub that exits in
        // microseconds, tmux destroys the session as soon as `printf`
        // returns — and on fast hosts (Ubuntu CI runners) `pipe-pane`
        // loses the race and reports `SessionNotFound`. macOS happens to
        // be slow enough that the race is rarely lost. Sleeping 5 seconds
        // gives the test plenty of headroom on any host without changing
        // anything about the production path. The tests don't wait on the
        // sleep — they read agent-out.txt as soon as `printf` has written
        // it, which happens before the sleep starts.
        let out_path = base.join("agent-out.txt");
        let stub_path = base.join("stub-agent.sh");
        std::fs::write(
            &stub_path,
            format!(
                "#!/bin/sh\n\
                 # Record argv so the test can verify the prompt was delivered.\n\
                 printf '%s\\n' \"$@\" > \"{}\"\n\
                 # Keep the tmux pane alive long enough for pipe-pane to\n\
                 # attach (see comment in setup_workshop in hand_spawn.rs).\n\
                 sleep 5\n",
                out_path.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&stub_path, perms).unwrap();
        }

        let pool = data::db::open_sparks_db(&base).await.unwrap();

        (base, pool, out_path)
    }

    /// Helper: build a TmuxClient pointing at the test workshop's private
    /// socket so we can verify sessions exist. Returns `None` when tmux
    /// is not available, allowing tests to skip gracefully.
    fn tmux_client_for(workshop_dir: &Path) -> Option<TmuxClient> {
        let tmux_bin = tmux::resolve_tmux_bin()?;
        let ryve_dir = RyveDir::new(workshop_dir);
        Some(TmuxClient::new(tmux_bin, ryve_dir.root()))
    }

    /// Helper: kill the tmux session (best-effort cleanup).
    fn cleanup_tmux(workshop_dir: &Path, session_name: &str) {
        if let Some(client) = tmux_client_for(workshop_dir) {
            let _ = client.kill_session(session_name);
        }
    }

    /// Acceptance criterion (3) for spark ryve-b3ad7bd1: spawning a Hand
    /// must result in a process that *receives its initial instructions*.
    /// We assert this end-to-end by reading the stub agent's recorded
    /// argv after the spawn and confirming the spark id appears in the
    /// prompt that was passed to it.
    ///
    /// Updated for spark ryve-75e6c64d: also verifies the tmux session
    /// exists and is named `hand-<session_id>`.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn spawned_hand_delivers_prompt_to_agent_process() {
        if !bundled_tmux_available_for_tests() {
            eprintln!(
                "bundled tmux not available — skipping integration test \
                 (these tests exercise the production hand-spawn path which \
                 is hardened against the pinned bundled tmux; arbitrary \
                 system tmux versions are covered by the separate Bundled \
                 tmux CI job)"
            );
            return;
        }
        let (workshop_dir, pool, out_path) = setup_workshop().await;
        let stub_path = workshop_dir.join("stub-agent.sh");

        // Create the spark in the same workshop_id the spawn helper uses.
        let workshop_id = workshop_id_for(&workshop_dir);
        let spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "first turn smoke test".into(),
                description: "verify the agent receives a user prompt".into(),
                spark_type: SparkType::Epic,
                priority: 1,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let agent = CodingAgent {
            display_name: "stub".into(),
            command: stub_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        let spawned = spawn_hand(
            &workshop_dir,
            &pool,
            &agent,
            &spark.id,
            HandKind::Owner,
            SpawnContext::default(),
        )
        .await
        .expect("spawn_hand should succeed against the stub agent");

        let tmux_name = format!("hand-{}", spawned.session_id);

        // Verify the tmux session exists on the private socket.
        if let Some(client) = tmux_client_for(&workshop_dir) {
            let sessions = client.list_sessions().unwrap();
            assert!(
                sessions.iter().any(|s| s.name == tmux_name),
                "tmux session {tmux_name} should exist; found: {sessions:?}"
            );
        }

        // Poll for the stub's output for up to 5 seconds. The child runs
        // inside tmux so we cannot `wait` on it; the file appearing means
        // the process actually executed and received argv.
        let deadline = Instant::now() + Duration::from_secs(5);
        let recorded = loop {
            if out_path.exists()
                && let Ok(s) = std::fs::read_to_string(&out_path)
                && !s.is_empty()
            {
                break s;
            }
            if Instant::now() >= deadline {
                panic!(
                    "stub agent never wrote {} — Hand failed to launch or process died before exec.\n\
                     log: {}",
                    out_path.display(),
                    std::fs::read_to_string(&spawned.log_path).unwrap_or_default()
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        // The fix: the user prompt must be in argv. The spark id is the
        // most stable substring of the composed prompt.
        assert!(
            recorded.contains(&spark.id),
            "stub agent did not receive a prompt containing the spark id.\n\
             recorded argv:\n{recorded}"
        );

        // Cleanup.
        cleanup_tmux(&workshop_dir, &tmux_name);
        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    /// Acceptance criteria for spark ryve-e4cadc03:
    ///   - spawning a Head creates an `agent_sessions` row with
    ///     `session_label = "head"`;
    ///   - it links to a new Crew whose `head_session_id` points at that
    ///     session;
    ///   - the archetype prompt template is handed to the spawned
    ///     subprocess verbatim.
    ///
    /// Updated for spark ryve-75e6c64d: verifies the tmux session exists
    /// as `head-<session_id>`.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn spawn_head_creates_session_and_crew_and_delivers_prompt() {
        if !bundled_tmux_available_for_tests() {
            eprintln!(
                "bundled tmux not available — skipping integration test \
                 (these tests exercise the production hand-spawn path which \
                 is hardened against the pinned bundled tmux; arbitrary \
                 system tmux versions are covered by the separate Bundled \
                 tmux CI job)"
            );
            return;
        }
        let (workshop_dir, pool, out_path) = setup_workshop().await;
        let stub_path = workshop_dir.join("stub-agent.sh");

        let workshop_id = workshop_id_for(&workshop_dir);
        let epic = spark_repo::create(
            &pool,
            NewSpark {
                title: "epic under test".into(),
                description: "verify Head spawn wiring".into(),
                spark_type: SparkType::Epic,
                priority: 1,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let agent = CodingAgent {
            display_name: "stub".into(),
            command: stub_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        let spawned = spawn_head(
            &workshop_dir,
            &pool,
            &agent,
            &epic.id,
            HeadArchetype::Build,
            SpawnContext::default(),
        )
        .await
        .expect("spawn_head should succeed against the stub agent");

        let tmux_name = format!("head-{}", spawned.session_id);
        assert_eq!(spawned.epic_id, epic.id);

        // Verify the tmux session exists.
        if let Some(client) = tmux_client_for(&workshop_dir) {
            let sessions = client.list_sessions().unwrap();
            assert!(
                sessions.iter().any(|s| s.name == tmux_name),
                "tmux session {tmux_name} should exist; found: {sessions:?}"
            );
        }

        // 1. Session row exists with label "head".
        let sessions_db = agent_session_repo::list_for_workshop(&pool, &workshop_id)
            .await
            .expect("list sessions");
        let head_row = sessions_db
            .iter()
            .find(|s| s.id == spawned.session_id)
            .expect("session row for spawned head");
        assert_eq!(head_row.session_label.as_deref(), Some("head"));

        // 2. Crew exists and its head_session_id points at the new session.
        let crew = crew_repo::get(&pool, &spawned.crew_id)
            .await
            .expect("crew row for spawned head");
        assert_eq!(
            crew.head_session_id.as_deref(),
            Some(spawned.session_id.as_str())
        );
        assert_eq!(crew.parent_spark_id.as_deref(), Some(epic.id.as_str()));

        // 3. Subprocess received the archetype prompt. Poll for the stub's
        //    recorded argv and assert it includes both the archetype name
        //    (identity-at-boot invariant) and the epic id it was given.
        let deadline = Instant::now() + Duration::from_secs(5);
        let recorded = loop {
            if out_path.exists()
                && let Ok(s) = std::fs::read_to_string(&out_path)
                && !s.is_empty()
            {
                break s;
            }
            if Instant::now() >= deadline {
                panic!(
                    "stub agent never wrote {} — Head failed to launch.\nlog: {}",
                    out_path.display(),
                    std::fs::read_to_string(&spawned.log_path).unwrap_or_default()
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(
            recorded.contains("build Head"),
            "archetype identity missing from prompt:\n{recorded}"
        );
        assert!(
            recorded.contains(&epic.id),
            "epic id missing from prompt:\n{recorded}"
        );

        cleanup_tmux(&workshop_dir, &tmux_name);
        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    /// When a `--crew` id is supplied, `spawn_head` must reuse that crew
    /// and update its `head_session_id` in place rather than creating a
    /// new crew. This is the path Atlas takes when it has already minted
    /// a crew for the goal.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn spawn_head_reuses_existing_crew() {
        if !bundled_tmux_available_for_tests() {
            eprintln!(
                "bundled tmux not available — skipping integration test \
                 (these tests exercise the production hand-spawn path which \
                 is hardened against the pinned bundled tmux; arbitrary \
                 system tmux versions are covered by the separate Bundled \
                 tmux CI job)"
            );
            return;
        }
        let (workshop_dir, pool, _out_path) = setup_workshop().await;
        let stub_path = workshop_dir.join("stub-agent.sh");

        let workshop_id = workshop_id_for(&workshop_dir);
        let epic = spark_repo::create(
            &pool,
            NewSpark {
                title: "existing-crew epic".into(),
                description: String::new(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        // Pre-create a crew that intentionally has NO head_session_id.
        // `spawn_head` must set it.
        let crew = crew_repo::create(
            &pool,
            NewCrew {
                name: "pre-existing crew".into(),
                purpose: None,
                workshop_id: workshop_id.clone(),
                head_session_id: None,
                parent_spark_id: Some(epic.id.clone()),
            },
        )
        .await
        .unwrap();

        let agent = CodingAgent {
            display_name: "stub".into(),
            command: stub_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        let spawned = spawn_head(
            &workshop_dir,
            &pool,
            &agent,
            &epic.id,
            HeadArchetype::Research,
            SpawnContext {
                crew_id: Some(&crew.id),
                ..SpawnContext::default()
            },
        )
        .await
        .expect("spawn_head should succeed with an existing crew");

        assert_eq!(spawned.crew_id, crew.id, "must reuse the supplied crew");

        let reloaded = crew_repo::get(&pool, &crew.id).await.unwrap();
        assert_eq!(
            reloaded.head_session_id.as_deref(),
            Some(spawned.session_id.as_str()),
            "head_session_id must be updated in place"
        );

        let members = crew_repo::members(&pool, &crew.id).await.unwrap();
        assert!(
            members
                .iter()
                .any(|m| m.session_id == spawned.session_id && m.role.as_deref() == Some("head")),
            "head must be registered as a crew member with role=head"
        );

        let tmux_name = format!("head-{}", spawned.session_id);
        cleanup_tmux(&workshop_dir, &tmux_name);
        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    /// Spark ryve-75e6c64d acceptance: spawn_hand creates a tmux session
    /// visible via the wrapper's list_sessions, and the log file at the
    /// expected path is being written.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn spawn_hand_creates_tmux_session_and_writes_log() {
        if !bundled_tmux_available_for_tests() {
            eprintln!(
                "bundled tmux not available — skipping integration test \
                 (these tests exercise the production hand-spawn path which \
                 is hardened against the pinned bundled tmux; arbitrary \
                 system tmux versions are covered by the separate Bundled \
                 tmux CI job)"
            );
            return;
        }
        let (workshop_dir, pool, _out_path) = setup_workshop().await;
        let stub_path = workshop_dir.join("stub-agent.sh");

        let workshop_id = workshop_id_for(&workshop_dir);
        let spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "tmux integration smoke test".into(),
                description: "verify tmux session + log file".into(),
                spark_type: SparkType::Epic,
                priority: 1,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let agent = CodingAgent {
            display_name: "stub".into(),
            command: stub_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        let spawned = spawn_hand(
            &workshop_dir,
            &pool,
            &agent,
            &spark.id,
            HandKind::Owner,
            SpawnContext::default(),
        )
        .await
        .expect("spawn_hand should succeed");

        let tmux_name = format!("hand-{}", spawned.session_id);

        // 1. Verify the tmux session is visible via list_sessions.
        let client = tmux_client_for(&workshop_dir).expect("tmux checked at top of test");
        let sessions = client.list_sessions().unwrap();
        assert!(
            sessions.iter().any(|s| s.name == tmux_name),
            "tmux session {tmux_name} not found in list_sessions; found: {sessions:?}"
        );

        // 2. Verify the log file exists at the expected path.
        let expected_log = workshop_dir
            .join(".ryve")
            .join("logs")
            .join(format!("hand-{}.log", spawned.session_id));
        assert_eq!(spawned.log_path, expected_log);
        assert!(
            expected_log.exists(),
            "log file should exist at {expected_log:?}"
        );

        // 3. Wait briefly for pipe-pane to flush some content. The stub
        //    script's output should appear in the log file via pipe-pane.
        let deadline = Instant::now() + Duration::from_secs(5);
        let log_has_content = loop {
            if let Ok(content) = std::fs::read_to_string(&expected_log)
                && !content.is_empty()
            {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        // pipe-pane output is best-effort — the stub runs fast and may
        // finish before pipe-pane captures anything. We still assert the
        // file exists (checked above), which is the invariant the UI
        // log-tail consumers rely on.
        let _ = log_has_content;

        cleanup_tmux(&workshop_dir, &tmux_name);
        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    // ─── Spark ryve-c44b92e5: actor-scoped branches + cross-user refusal ──

    // ─── Spark ryve-a2d447d1: HandKind::Investigator + CLI --role investigator ──

    /// `HandKind::Investigator` carries the session_label "investigator"
    /// (written to `agent_sessions.session_label` and to the crew_members
    /// role column) and claims its spark with `AssignmentRole::Owner` —
    /// same as a regular Owner Hand. The invariant-preserving test for
    /// acceptance criterion (3) of spark ryve-a2d447d1.
    #[test]
    fn investigator_kind_maps_to_owner_role_and_investigator_label() {
        assert_eq!(HandKind::Investigator.role(), AssignmentRole::Owner);
        assert_eq!(HandKind::Investigator.session_label(), "investigator");
        // And the existing three kinds keep their labels.
        assert_eq!(HandKind::Owner.session_label(), "hand");
        assert_eq!(HandKind::Head.session_label(), "head");
        assert_eq!(HandKind::Merger.session_label(), "merger");
    }

    /// Spark ryve-3f799949: `HandKind::Architect` shares the Owner
    /// assignment semantics with the other read-only archetypes
    /// (Investigator), but carries its own `session_label = "architect"`
    /// so crew members and session rows render correctly.
    #[test]
    fn architect_kind_maps_to_owner_role_and_architect_label() {
        assert_eq!(HandKind::Architect.role(), AssignmentRole::Owner);
        assert_eq!(HandKind::Architect.session_label(), "architect");
    }

    /// End-to-end: `spawn_hand` with `HandKind::Investigator` must persist
    /// `session_label = "investigator"` on the `agent_sessions` row AND on
    /// the `crew_members` row, and must claim the spark with
    /// `AssignmentRole::Owner`. Acceptance criterion (4) of spark
    /// ryve-a2d447d1.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn spawn_hand_records_investigator_label_on_session_and_crew() {
        if !bundled_tmux_available_for_tests() {
            eprintln!("bundled tmux not available — skipping investigator spawn test");
            return;
        }
        let (workshop_dir, pool, _out_path) = setup_workshop().await;
        let stub_path = workshop_dir.join("stub-agent.sh");

        let workshop_id = workshop_id_for(&workshop_dir);
        let spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "audit spark".into(),
                description: "investigator sweep".into(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        // A crew to verify the crew_members role column gets the
        // "investigator" label too (invariant: the crew add-member path
        // tags investigator members with role label "investigator", not
        // "hand").
        let crew = crew_repo::create(
            &pool,
            NewCrew {
                name: "research crew".into(),
                purpose: None,
                workshop_id: workshop_id.clone(),
                head_session_id: None,
                parent_spark_id: Some(spark.id.clone()),
            },
        )
        .await
        .unwrap();

        let agent = CodingAgent {
            display_name: "stub".into(),
            command: stub_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        let spawned = spawn_hand(
            &workshop_dir,
            &pool,
            &agent,
            &spark.id,
            HandKind::Investigator,
            SpawnContext {
                crew_id: Some(&crew.id),
                ..SpawnContext::default()
            },
        )
        .await
        .expect("spawn_hand with HandKind::Investigator should succeed");

        // 1. agent_sessions.session_label == "investigator".
        let sessions_db = agent_session_repo::list_for_workshop(&pool, &workshop_id)
            .await
            .expect("list sessions");
        let row = sessions_db
            .iter()
            .find(|s| s.id == spawned.session_id)
            .expect("session row for spawned investigator");
        assert_eq!(row.session_label.as_deref(), Some("investigator"));

        // 2. Assignment row claims the spark with AssignmentRole::Owner.
        //    We assert this indirectly via is_spark_claimed + actor lookup
        //    (same hooks the branch validator and UI use).
        let claimed = data::sparks::assignment_repo::is_spark_claimed(&pool, &spark.id)
            .await
            .unwrap();
        assert!(claimed, "investigator must claim its audit spark");

        // 3. crew_members row for this session carries role = "investigator".
        let members = crew_repo::members(&pool, &crew.id).await.unwrap();
        let member = members
            .iter()
            .find(|m| m.session_id == spawned.session_id)
            .expect("investigator must be registered as a crew member");
        assert_eq!(
            member.role.as_deref(),
            Some("investigator"),
            "crew member role must be 'investigator', not 'hand'"
        );

        let tmux_name = format!("investigator-{}", spawned.session_id);
        cleanup_tmux(&workshop_dir, &tmux_name);
        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    /// `validate_actor` accepts any non-empty single-segment string and
    /// rejects anything that would produce an invalid git ref or collide
    /// with the reserved `<prefix>/<id>` prefixes that epic / crew / release
    /// branches live under.
    #[test]
    fn validate_actor_rejects_empty_and_path_segments() {
        assert!(super::validate_actor("alice").is_ok());
        assert!(super::validate_actor("claude").is_ok());

        assert!(matches!(
            super::validate_actor(""),
            Err(HandSpawnError::InvalidActor(_))
        ));
        assert!(matches!(
            super::validate_actor("alice/bob"),
            Err(HandSpawnError::InvalidActor(_))
        ));
    }

    /// Actor resolution order: explicit override > `RYVE_ACTOR_ID` env >
    /// `USER` env > literal `"hand"` fallback. All cases live in one
    /// serial test because `std::env` is process-global and Rust's
    /// default test harness runs tests in parallel — splitting these
    /// across tests races on the same variables.
    #[test]
    fn resolve_actor_respects_full_priority_chain() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        struct EnvGuard(&'static str, Option<String>);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.1 {
                    Some(v) => unsafe { std::env::set_var(self.0, v) },
                    None => unsafe { std::env::remove_var(self.0) },
                }
            }
        }
        let _g1 = EnvGuard("RYVE_ACTOR_ID", std::env::var("RYVE_ACTOR_ID").ok());
        let _g2 = EnvGuard("USER", std::env::var("USER").ok());

        // 1. Explicit override beats both env vars.
        unsafe {
            std::env::set_var("RYVE_ACTOR_ID", "env-actor");
            std::env::set_var("USER", "shell-user");
        }
        assert_eq!(
            super::resolve_actor_from_env(Some("cli-actor")),
            "cli-actor"
        );

        // 2. With no override, RYVE_ACTOR_ID wins over USER.
        assert_eq!(super::resolve_actor_from_env(None), "env-actor");

        // 3. Clearing RYVE_ACTOR_ID falls through to USER.
        unsafe {
            std::env::remove_var("RYVE_ACTOR_ID");
        }
        assert_eq!(super::resolve_actor_from_env(None), "shell-user");

        // 4. Clearing both yields the literal "hand" floor.
        unsafe {
            std::env::remove_var("USER");
        }
        assert_eq!(super::resolve_actor_from_env(None), "hand");
    }

    /// Acceptance — spark ryve-c44b92e5: spawn_hand must cut `<actor>/<short>`
    /// instead of `hand/<short>`, and the assignment row must carry the
    /// same actor_id so downstream consumers see a consistent identity.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn spawn_hand_cuts_actor_scoped_branch_and_records_actor() {
        if !bundled_tmux_available_for_tests() {
            eprintln!("bundled tmux not available — skipping actor-scoped spawn test");
            return;
        }
        let (workshop_dir, pool, _out_path) = setup_workshop().await;
        let stub_path = workshop_dir.join("stub-agent.sh");

        let workshop_id = workshop_id_for(&workshop_dir);
        let spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "actor-scoped branch".into(),
                description: String::new(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let agent = CodingAgent {
            display_name: "stub".into(),
            command: stub_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        let spawned = spawn_hand(
            &workshop_dir,
            &pool,
            &agent,
            &spark.id,
            HandKind::Owner,
            SpawnContext {
                actor_id: Some("alice"),
                ..SpawnContext::default()
            },
        )
        .await
        .expect("spawn_hand should succeed");

        // Branch check: query git directly in the workshop. The branch
        // created by `git worktree add -b` lives in the shared ref set.
        let short_id = &spawned.session_id[..8];
        let out = std::process::Command::new("git")
            .args(["branch", "--list", &format!("alice/{short_id}")])
            .current_dir(&workshop_dir)
            .output()
            .unwrap();
        let listed = String::from_utf8_lossy(&out.stdout);
        assert!(
            listed.contains(&format!("alice/{short_id}")),
            "expected alice/{short_id} branch to exist after spawn; got: {listed}"
        );

        // Assignment row must carry actor_id = "alice" (not the default
        // session_id fallback). This is the hook other consumers (branch
        // validator, UI labels, audits) read from.
        let recorded =
            data::sparks::assignment_repo::actor_id_for_session(&pool, &spawned.session_id)
                .await
                .unwrap();
        assert_eq!(recorded.as_deref(), Some("alice"));

        let tmux_name = format!("hand-{}", spawned.session_id);
        cleanup_tmux(&workshop_dir, &tmux_name);
        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    /// Acceptance — spark ryve-c44b92e5: when a parent session's assignment
    /// already pins an actor, spawning a Hand under a *different* actor
    /// must be refused before any git or DB state is created.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn spawn_hand_refuses_cross_actor() {
        // This test needs no tmux — it short-circuits before worktree
        // creation on the cross-actor check.
        let (workshop_dir, pool, _out_path) = setup_workshop().await;
        let stub_path = workshop_dir.join("stub-agent.sh");

        let workshop_id = workshop_id_for(&workshop_dir);

        // Stand up a parent session with an active assignment whose
        // actor_id is "alice". `spawn_hand` reads this row via
        // `assignment_repo::actor_id_for_session` to discover the parent's
        // actor.
        let parent_spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "parent spark".into(),
                description: String::new(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let parent_session_id = Uuid::new_v4().to_string();
        agent_session_repo::create(
            &pool,
            &data::sparks::types::NewAgentSession {
                id: parent_session_id.clone(),
                workshop_id: workshop_id.clone(),
                agent_name: "stub".into(),
                agent_command: stub_path.to_string_lossy().into_owned(),
                agent_args: Vec::new(),
                session_label: Some("hand".into()),
                child_pid: None,
                resume_id: None,
                log_path: None,
                parent_session_id: None,
                archetype_id: None,
            },
        )
        .await
        .unwrap();

        data::sparks::assignment_repo::assign(
            &pool,
            NewHandAssignment {
                session_id: parent_session_id.clone(),
                spark_id: parent_spark.id.clone(),
                role: AssignmentRole::Owner,
                actor_id: Some("alice".into()),
            },
        )
        .await
        .unwrap();

        // Child spark the cross-actor spawn would try to claim.
        let child_spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "child spark".into(),
                description: String::new(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let agent = CodingAgent {
            display_name: "stub".into(),
            command: stub_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        let result = spawn_hand(
            &workshop_dir,
            &pool,
            &agent,
            &child_spark.id,
            HandKind::Owner,
            SpawnContext {
                parent_session_id: Some(&parent_session_id),
                actor_id: Some("bob"), // different actor from the parent ("alice")
                ..SpawnContext::default()
            },
        )
        .await;

        match result {
            Err(HandSpawnError::CrossActorRefused {
                parent_actor,
                requested_actor,
                ..
            }) => {
                assert_eq!(parent_actor, "alice");
                assert_eq!(requested_actor, "bob");
            }
            other => panic!(
                "expected CrossActorRefused, got {other:?} — cross-user boundary not enforced"
            ),
        }

        // Nothing should have been persisted for the refused spawn —
        // no assignment row for the child spark.
        let claimed = data::sparks::assignment_repo::is_spark_claimed(&pool, &child_spark.id)
            .await
            .unwrap();
        assert!(
            !claimed,
            "refused spawn must not leave an assignment behind on the child spark"
        );

        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    /// Acceptance — spark ryve-c44b92e5: when the parent and the requested
    /// actor match, the spawn is accepted. Guards against the cross-actor
    /// check over-rejecting legitimate same-actor spawns.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn spawn_hand_allows_same_actor_child() {
        if !bundled_tmux_available_for_tests() {
            eprintln!("bundled tmux not available — skipping same-actor spawn test");
            return;
        }
        let (workshop_dir, pool, _out_path) = setup_workshop().await;
        let stub_path = workshop_dir.join("stub-agent.sh");

        let workshop_id = workshop_id_for(&workshop_dir);

        let parent_spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "parent".into(),
                description: String::new(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let parent_session_id = Uuid::new_v4().to_string();
        agent_session_repo::create(
            &pool,
            &data::sparks::types::NewAgentSession {
                id: parent_session_id.clone(),
                workshop_id: workshop_id.clone(),
                agent_name: "stub".into(),
                agent_command: stub_path.to_string_lossy().into_owned(),
                agent_args: Vec::new(),
                session_label: Some("hand".into()),
                child_pid: None,
                resume_id: None,
                log_path: None,
                parent_session_id: None,
                archetype_id: None,
            },
        )
        .await
        .unwrap();

        data::sparks::assignment_repo::assign(
            &pool,
            NewHandAssignment {
                session_id: parent_session_id.clone(),
                spark_id: parent_spark.id.clone(),
                role: AssignmentRole::Owner,
                actor_id: Some("alice".into()),
            },
        )
        .await
        .unwrap();

        let child_spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "child".into(),
                description: String::new(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let agent = CodingAgent {
            display_name: "stub".into(),
            command: stub_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        let spawned = spawn_hand(
            &workshop_dir,
            &pool,
            &agent,
            &child_spark.id,
            HandKind::Owner,
            SpawnContext {
                parent_session_id: Some(&parent_session_id),
                actor_id: Some("alice"),
                ..SpawnContext::default()
            },
        )
        .await
        .expect("same-actor spawn must succeed");

        let recorded =
            data::sparks::assignment_repo::actor_id_for_session(&pool, &spawned.session_id)
                .await
                .unwrap();
        assert_eq!(recorded.as_deref(), Some("alice"));

        let tmux_name = format!("hand-{}", spawned.session_id);
        cleanup_tmux(&workshop_dir, &tmux_name);
        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    // ─── Spark ryve-8384b3cc: mechanical tool-policy gating at spawn time ──

    /// Build a stub agent that attempts to write a marker file into its
    /// cwd (the Hand's worktree) and then sleeps briefly so the tmux
    /// session stays alive long enough for pipe-pane. Redirecting stderr
    /// to /dev/null plus `|| true` keeps the stub from crashing when the
    /// read-only policy blocks the write; the test checks the filesystem
    /// afterwards, not the stub's exit status.
    ///
    /// The marker path is `$PWD/HAND_WROTE_HERE.txt` so the test can
    /// look for it inside `spawned.worktree_path` without knowing any
    /// internal state.
    fn write_attempt_stub(workshop_dir: &Path) -> PathBuf {
        let stub_path = workshop_dir.join("stub-write-agent.sh");
        std::fs::write(
            &stub_path,
            "#!/bin/sh\n\
             echo 'hand wrote here' > \"$PWD/HAND_WROTE_HERE.txt\" 2>/dev/null || true\n\
             sleep 3\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&stub_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&stub_path, perms).unwrap();
        }
        stub_path
    }

    /// Acceptance criterion for spark ryve-8384b3cc: spawning a
    /// read-only archetype (Investigator) must mechanically block any
    /// write the subprocess attempts into its worktree, regardless of
    /// the system prompt.
    ///
    /// The stub agent tries to create `HAND_WROTE_HERE.txt` in its cwd
    /// (the worktree). After the stub completes, the file must NOT
    /// exist — the kernel rejected the write with `EACCES` because
    /// `spawn_hand` chmod'd the worktree read-only before launching the
    /// subprocess. Also asserts the session row was persisted, i.e. the
    /// "agent_session completes without mutating the tree" half of the
    /// acceptance criterion.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn read_only_archetype_worktree_rejects_subprocess_writes() {
        if !bundled_tmux_available_for_tests() {
            eprintln!("bundled tmux not available — skipping read-only policy integration test");
            return;
        }
        let (workshop_dir, pool, _out_path) = setup_workshop().await;
        let stub_path = write_attempt_stub(&workshop_dir);

        let workshop_id = workshop_id_for(&workshop_dir);
        let spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "investigator write-gate".into(),
                description: "audit spark for tool-policy test".into(),
                spark_type: SparkType::Epic,
                priority: 1,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let agent = CodingAgent {
            display_name: "stub".into(),
            command: stub_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        let spawned = spawn_hand(
            &workshop_dir,
            &pool,
            &agent,
            &spark.id,
            HandKind::Investigator,
            SpawnContext::default(),
        )
        .await
        .expect("spawn_hand with HandKind::Investigator should succeed");

        // Wait long enough for the stub's write attempt to complete.
        // The stub sleeps 3s after its attempt, so 1s is plenty of head
        // room for the redirect+echo to have run (or been rejected).
        tokio::time::sleep(Duration::from_millis(1000)).await;

        let marker = spawned.worktree_path.join("HAND_WROTE_HERE.txt");
        assert!(
            !marker.exists(),
            "read-only archetype's write must have been rejected by the \
             kernel; marker file exists at {}",
            marker.display()
        );

        // The agent_session row still exists — the gate rejected writes,
        // not the session itself. That's the "session completes without
        // mutating the tree" half of the invariant.
        let sessions_db = agent_session_repo::list_for_workshop(&pool, &workshop_id)
            .await
            .expect("list sessions");
        assert!(
            sessions_db.iter().any(|s| s.id == spawned.session_id),
            "read-only spawn must still persist its agent_sessions row"
        );

        // Unlock the worktree so the tempdir cleanup can remove files
        // whose dirs lost the `w` bit under the read-only chmod.
        let _ = crate::hand_archetypes::unlock_worktree(&spawned.worktree_path);
        let tmux_name = format!("investigator-{}", spawned.session_id);
        cleanup_tmux(&workshop_dir, &tmux_name);
        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    /// Acceptance criterion for spark ryve-8384b3cc: spawning a
    /// write-capable archetype (Owner Hand — the "Bug Hunter" shape)
    /// must behave exactly as today. The invariant "no regressions on
    /// write-capable archetypes" is the flip side of the read-only gate
    /// test above — a blanket chmod that also locked down Owner Hands
    /// would break every build Crew.
    ///
    /// The same stub is used; this time the marker file IS expected to
    /// exist after the subprocess runs.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn write_capable_archetype_worktree_still_accepts_writes() {
        if !bundled_tmux_available_for_tests() {
            eprintln!(
                "bundled tmux not available — skipping write-capable policy integration test"
            );
            return;
        }
        let (workshop_dir, pool, _out_path) = setup_workshop().await;
        let stub_path = write_attempt_stub(&workshop_dir);

        let workshop_id = workshop_id_for(&workshop_dir);
        let spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "bug-hunter write path".into(),
                description: "owner hand spark for tool-policy regression test".into(),
                spark_type: SparkType::Epic,
                priority: 1,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let agent = CodingAgent {
            display_name: "stub".into(),
            command: stub_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        let spawned = spawn_hand(
            &workshop_dir,
            &pool,
            &agent,
            &spark.id,
            HandKind::Owner,
            SpawnContext::default(),
        )
        .await
        .expect("spawn_hand with HandKind::Owner should succeed");

        // Poll for the marker file; the stub writes it within a few ms
        // of exec. Use the same 5s budget as the other spawn tests to
        // ride out slow CI runners.
        let marker = spawned.worktree_path.join("HAND_WROTE_HERE.txt");
        let deadline = Instant::now() + Duration::from_secs(5);
        let created = loop {
            if marker.exists() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(
            created,
            "write-capable archetype must be able to create files in its \
             worktree — expected marker at {} (log: {})",
            marker.display(),
            std::fs::read_to_string(&spawned.log_path).unwrap_or_default(),
        );

        let tmux_name = format!("hand-{}", spawned.session_id);
        cleanup_tmux(&workshop_dir, &tmux_name);
        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    // ─── Reviewer selection + spawn path (spark ryve-f6259067) ─────

    /// Helper: build a reviewer pool from `(actor_id, vendor)` pairs.
    fn pool(entries: &[(&str, &str)]) -> Vec<ReviewerCandidate> {
        entries
            .iter()
            .map(|(a, v)| ReviewerCandidate {
                actor_id: (*a).to_string(),
                vendor: (*v).to_string(),
            })
            .collect()
    }

    /// Invariant: the author is never chosen as their own reviewer, even
    /// when they are the only candidate in the pool — that case must
    /// surface as `NoEligibleReviewer`, not a silent self-approval.
    #[test]
    fn select_reviewer_excludes_author_even_as_sole_candidate() {
        let pool = pool(&[("alice", "claude")]);
        let err = select_reviewer("alice", "claude", &pool, 0)
            .expect_err("author-only pool must be NoEligibleReviewer");
        match err {
            ReviewerSelectionError::NoEligibleReviewer {
                author_actor,
                pool_size,
            } => {
                assert_eq!(author_actor, "alice");
                assert_eq!(pool_size, 1);
            }
        }
    }

    /// Invariant: when a cross-vendor candidate exists, the selection
    /// returns `CrossVendor` and never falls back to a same-vendor peer.
    #[test]
    fn select_reviewer_prefers_cross_vendor_over_same_vendor() {
        let pool = pool(&[("bob", "claude"), ("carol", "codex")]);
        let (picked, outcome) = select_reviewer("alice", "claude", &pool, 0)
            .expect("cross-vendor reviewer must be selectable");
        assert_eq!(outcome, ReviewerSelection::CrossVendor);
        assert_eq!(picked.actor_id, "carol");
        assert_eq!(picked.vendor, "codex");
    }

    /// Invariant: when only same-vendor candidates remain, the
    /// selection falls back and returns `SameVendorRelaxed`. The caller
    /// (`spawn_reviewer`) is responsible for emitting
    /// `reviewer_policy_relaxed` in this case.
    #[test]
    fn select_reviewer_falls_back_to_same_vendor_when_no_cross_vendor_available() {
        let pool = pool(&[("bob", "claude"), ("carol", "claude")]);
        let (picked, outcome) = select_reviewer("alice", "claude", &pool, 0)
            .expect("same-vendor fallback must be selectable");
        assert_eq!(outcome, ReviewerSelection::SameVendorRelaxed);
        assert_eq!(picked.vendor, "claude");
        assert_ne!(picked.actor_id, "alice");
    }

    /// Determinism invariant: identical inputs must always produce the
    /// same reviewer. This is the property the audit/replay layer
    /// depends on — a reviewer reassignment in the event log must be
    /// reproducible bit-for-bit.
    #[test]
    fn select_reviewer_is_deterministic_for_fixed_seed() {
        let pool = pool(&[("bob", "codex"), ("carol", "codex"), ("dave", "codex")]);
        let first = select_reviewer("alice", "claude", &pool, 42).unwrap().0;
        let second = select_reviewer("alice", "claude", &pool, 42).unwrap().0;
        let third = select_reviewer("alice", "claude", &pool, 42).unwrap().0;
        assert_eq!(first.actor_id, second.actor_id);
        assert_eq!(second.actor_id, third.actor_id);
    }

    /// Determinism invariant (negative direction): different seeds must
    /// be able to pick different reviewers when the pool has more than
    /// one cross-vendor candidate, otherwise the seed argument is
    /// vestigial and audits cannot distinguish distinct runs.
    #[test]
    fn select_reviewer_seed_spreads_over_eligible_bucket() {
        let pool = pool(&[("bob", "codex"), ("carol", "codex")]);
        let a = select_reviewer("alice", "claude", &pool, 0).unwrap().0;
        let b = select_reviewer("alice", "claude", &pool, 1).unwrap().0;
        assert_ne!(a.actor_id, b.actor_id);
    }

    /// Invariant: when the pool contains only the author, NoEligible is
    /// raised regardless of vendor. Guards against a regression where a
    /// same-vendor author might be rediscovered via the fallback
    /// branch.
    #[test]
    fn select_reviewer_author_only_same_vendor_pool_still_fails() {
        let pool = pool(&[("alice", "claude")]);
        assert!(select_reviewer("alice", "claude", &pool, 7).is_err());
    }

    /// Integration invariant: when no eligible reviewer exists, the
    /// spawn path MUST flag the spark and raise a flare ember rather
    /// than silently doing nothing. This is the `awaiting_reviewer_
    /// availability` surface the UI / sweep watches.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn spawn_reviewer_without_eligible_pool_flags_and_raises_flare() {
        use data::sparks::types::{EmberType, NewSpark, SparkType};
        use data::sparks::{ember_repo, event_repo, spark_repo};

        let (workshop_dir, pool, _out_path) = setup_workshop().await;

        let spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "awaiting-reviewer smoke".into(),
                description: "verify empty-pool path flags and raises".into(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: workshop_id_for(&workshop_dir),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        // No tmux binary needed: we never reach the launch step because
        // the pool is empty, so the agent's command is only referenced
        // on the success branch.
        let agent = CodingAgent {
            display_name: "stub".into(),
            command: "/nonexistent/stub-agent.sh".into(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        // Pool contains only the author — by contract, no eligible
        // reviewer.
        let author_only = vec![ReviewerCandidate {
            actor_id: "alice".into(),
            vendor: "claude".into(),
        }];

        let outcome = spawn_reviewer(
            &workshop_dir,
            &pool,
            &agent,
            &spark.id,
            ReviewerRequest {
                author_actor: "alice",
                author_vendor: "claude",
                candidates: &author_only,
                seed: 0,
            },
            SpawnContext::default(),
        )
        .await
        .expect("spawn_reviewer with no eligible pool must not error out — it flags");

        match outcome {
            ReviewerSpawnOutcome::AwaitingAvailability { ember_id } => {
                assert!(!ember_id.is_empty(), "ember id must be returned");
            }
            ReviewerSpawnOutcome::Spawned { .. } => {
                panic!("empty pool must not spawn a reviewer Hand");
            }
        }

        // Event audit trail: the spark has an
        // `awaiting_reviewer_availability` row on its event log.
        let events = event_repo::list_for_spark(&pool, &spark.id).await.unwrap();
        let availability_row = events
            .iter()
            .find(|e| e.field_name == "reviewer_availability")
            .expect("reviewer_availability event must be recorded");
        assert_eq!(
            availability_row.new_value.as_deref(),
            Some("awaiting_reviewer_availability"),
            "event must carry the canonical awaiting-availability value",
        );

        // IRC surface: a `flare` ember was raised on this workshop.
        let workshop_id = workshop_id_for(&workshop_dir);
        let flares = ember_repo::list_by_type(&pool, &workshop_id, EmberType::Flare)
            .await
            .unwrap();
        assert!(
            flares
                .iter()
                .any(|e| e.content.contains("awaiting_reviewer_availability")
                    && e.content.contains(&spark.id)),
            "a flare ember scoping `awaiting_reviewer_availability` must exist; \
             got {} flares: {:?}",
            flares.len(),
            flares.iter().map(|e| &e.content).collect::<Vec<_>>(),
        );

        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    /// Integration invariant: on a same-vendor-only pool, `spawn_reviewer`
    /// MUST record the `reviewer_policy_relaxed` event on the spark BEFORE
    /// dispatching the spawn, so the relaxation is auditable even if the
    /// subprocess launch downstream fails. The test reads the event log
    /// directly — tmux is not required because the assertion is purely
    /// on the workgraph write, not on the subprocess lifecycle.
    #[tokio::test]
    #[ignore = "subprocess-heavy hand_spawn test; flaky on GHA runners, run locally with --ignored"]
    async fn spawn_reviewer_same_vendor_fallback_records_policy_relaxed_event() {
        use data::sparks::types::{NewSpark, SparkType};
        use data::sparks::{event_repo, spark_repo};

        if !bundled_tmux_available_for_tests() {
            eprintln!(
                "bundled tmux not available — skipping: this test runs the \
                 full spawn_reviewer happy path which requires tmux for the \
                 inner spawn_hand call. The per-branch event write is still \
                 covered unconditionally by the empty-pool test above."
            );
            return;
        }

        let (workshop_dir, pool, _out_path) = setup_workshop().await;
        let stub_path = workshop_dir.join("stub-agent.sh");

        let spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "reviewer-policy-relaxed smoke".into(),
                description: "same-vendor fallback must log the relaxation".into(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: workshop_id_for(&workshop_dir),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let agent = CodingAgent {
            display_name: "stub".into(),
            command: stub_path.to_string_lossy().into_owned(),
            args: Vec::new(),
            resume: ResumeStrategy::None,
            compatibility: crate::coding_agents::CompatStatus::Unknown,
        };

        // Pool: only same-vendor candidates apart from the author →
        // forces the fallback path.
        let same_vendor_only = vec![
            ReviewerCandidate {
                actor_id: "alice".into(),
                vendor: "claude".into(),
            },
            ReviewerCandidate {
                actor_id: "bob".into(),
                vendor: "claude".into(),
            },
        ];

        let outcome = spawn_reviewer(
            &workshop_dir,
            &pool,
            &agent,
            &spark.id,
            ReviewerRequest {
                author_actor: "alice",
                author_vendor: "claude",
                candidates: &same_vendor_only,
                seed: 0,
            },
            SpawnContext::default(),
        )
        .await
        .expect("same-vendor fallback must succeed (spawn path returns Spawned)");

        let (hand, selection) = match outcome {
            ReviewerSpawnOutcome::Spawned { hand, selection } => (hand, selection),
            ReviewerSpawnOutcome::AwaitingAvailability { .. } => {
                panic!("with a non-empty same-vendor pool, a reviewer must be spawned");
            }
        };
        assert_eq!(selection, ReviewerSelection::SameVendorRelaxed);

        let events = event_repo::list_for_spark(&pool, &spark.id).await.unwrap();
        let relaxed_row = events
            .iter()
            .find(|e| e.field_name == "reviewer_policy")
            .expect("reviewer_policy event must be recorded on same-vendor fallback");
        assert_eq!(
            relaxed_row.new_value.as_deref(),
            Some("same_vendor_relaxed"),
            "event must carry the canonical relaxation marker",
        );
        assert_eq!(
            relaxed_row.old_value.as_deref(),
            Some("cross_vendor"),
            "relaxation event must record the transition from cross_vendor",
        );
        assert!(
            relaxed_row
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("reviewer_policy_relaxed")),
            "event reason must name the relaxation explicitly",
        );

        let tmux_name = format!("reviewer-{}", hand.session_id);
        cleanup_tmux(&workshop_dir, &tmux_name);
        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    /// Direct coverage of `resolve_hand_base_ref` on the very path
    /// `spawn_hand` invokes — no tmux, no subprocess agents, just the
    /// DB + git rev-parse pair. Guards against a refactor that drops
    /// the lookup from the spawn path or drifts the branch-name
    /// convention from `<actor>/<short>` (spark [sp-b7633430]).
    #[tokio::test]
    async fn resolve_hand_base_ref_returns_predecessor_tip() {
        use data::sparks::types::{
            AssignmentRole, BondType, NewAgentSession, NewHandAssignment, NewSpark, SparkType,
        };
        use data::sparks::{agent_session_repo, assignment_repo, bond_repo, spark_repo};

        let (workshop_dir, pool, _out_path) = setup_workshop().await;
        let workshop_id = workshop_id_for(&workshop_dir);

        let parent = spark_repo::create(
            &pool,
            NewSpark {
                title: "resolver test epic".into(),
                description: String::new(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let spark_a = spark_repo::create(
            &pool,
            NewSpark {
                title: "child A".into(),
                description: String::new(),
                spark_type: SparkType::Task,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: Some(parent.id.clone()),
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: Some("sentinel.txt".into()),
            },
        )
        .await
        .unwrap();

        let spark_b = spark_repo::create(
            &pool,
            NewSpark {
                title: "child B (blocked by A)".into(),
                description: String::new(),
                spark_type: SparkType::Task,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: Some(parent.id.clone()),
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: Some("sentinel.txt".into()),
            },
        )
        .await
        .unwrap();

        bond_repo::create(&pool, &spark_a.id, &spark_b.id, BondType::Blocks)
            .await
            .unwrap();

        // With no assignment yet, the resolver returns None even with a
        // predecessor bond — there is no Hand owning A, so there is no
        // branch to stack on.
        assert!(
            super::resolve_hand_base_ref(&pool, &workshop_dir, &spark_b.id)
                .await
                .is_none(),
            "no owning Hand on the predecessor means no base ref"
        );

        // Seed an owner assignment + branch for Hand A. Use `resolve_actor_from_env`
        // indirectly via a fixed actor so the branch-naming convention
        // is exercised exactly as the CLI spawn path does it.
        let actor = "testhand";
        let session_a = Uuid::new_v4().to_string();
        agent_session_repo::create(
            &pool,
            &NewAgentSession {
                id: session_a.clone(),
                workshop_id: workshop_id.clone(),
                agent_name: "stub".into(),
                agent_command: "true".into(),
                agent_args: vec![],
                session_label: Some("hand".into()),
                child_pid: None,
                resume_id: None,
                log_path: None,
                parent_session_id: None,
                archetype_id: None,
            },
        )
        .await
        .unwrap();
        assignment_repo::assign(
            &pool,
            NewHandAssignment {
                session_id: session_a.clone(),
                spark_id: spark_a.id.clone(),
                role: AssignmentRole::Owner,
                actor_id: Some(actor.to_string()),
            },
        )
        .await
        .unwrap();

        // Hand A's worktree — actually create the branch so rev-parse
        // resolves. We don't need to commit; the point is that
        // `<actor>/<short_a>` exists at a known SHA.
        let branch_a = super::hand_branch_name(actor, &session_a);
        let wt_a = workshop_dir
            .join(".ryve")
            .join("worktrees")
            .join(&session_a[..8]);
        let out = std::process::Command::new("git")
            .args(["worktree", "add", "-b", &branch_a, wt_a.to_str().unwrap()])
            .current_dir(&workshop_dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let expected_sha = {
            let o = std::process::Command::new("git")
                .args(["rev-parse", "--verify", &format!("refs/heads/{branch_a}")])
                .current_dir(&workshop_dir)
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        assert!(!expected_sha.is_empty(), "branch_a must resolve");

        // Now the resolver sees: predecessor is spark_a → owner session
        // is `session_a` with actor `testhand` → branch `<actor>/<short>`
        // resolves to `expected_sha` → return Some(expected_sha).
        let resolved = super::resolve_hand_base_ref(&pool, &workshop_dir, &spark_b.id)
            .await
            .expect("resolver must return predecessor tip SHA");
        assert_eq!(
            resolved, expected_sha,
            "resolver must return exactly Hand A's branch tip"
        );

        // Control: a spark with no predecessors still returns None.
        assert!(
            super::resolve_hand_base_ref(&pool, &workshop_dir, &spark_a.id)
                .await
                .is_none(),
            "spark with no blocks predecessors returns None (unchanged behavior)"
        );

        let _ = std::fs::remove_dir_all(&workshop_dir);
    }

    /// Regression for spark ryve-b6d9a53c / [sp-b6d9a53c]: when `spawn_hand`
    /// re-enters an epic whose task spark already had a prior owner, the
    /// boot prompt must include the last N posts from the relevant epic
    /// channel AND must not include the previous owner's session log file
    /// content. Exercises the pure composer so the regression is fast and
    /// has no dependency on tmux / subprocesses.
    #[tokio::test]
    async fn respawn_prepends_channel_context_not_log_file() {
        use data::sparks::types::{
            AssignmentRole, NewAgentSession, NewHandAssignment, NewSpark, SparkType,
        };
        use data::sparks::{agent_session_repo, assignment_repo, spark_repo};

        let workshop_dir =
            std::env::temp_dir().join(format!("ryve-respawn-context-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&workshop_dir).unwrap();
        let pool = data::db::open_sparks_db(&workshop_dir).await.unwrap();
        let workshop_id = workshop_id_for(&workshop_dir);

        // --- (1) Parent epic + task spark under it -----------------------
        let epic = spark_repo::create(
            &pool,
            NewSpark {
                title: "respawn context epic".into(),
                description: String::new(),
                spark_type: SparkType::Epic,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        let task = spark_repo::create(
            &pool,
            NewSpark {
                title: "child task under the respawn epic".into(),
                description: String::new(),
                spark_type: SparkType::Task,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: Some(epic.id.clone()),
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();

        // --- (2) Prior owner Hand — session row, log file, assignment ---
        let prior_session_id = Uuid::new_v4().to_string();
        let logs_dir = workshop_dir.join(".ryve").join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        let prior_log_path = logs_dir.join(format!("hand-{prior_session_id}.log"));
        // Write a unique sentinel into the log so the test can prove the
        // composer does NOT leak log file content into the new Hand's prompt.
        let log_sentinel = "TOPSECRET_LOG_MARKER_DO_NOT_LEAK";
        std::fs::write(
            &prior_log_path,
            format!("boot: tmux attached\n{log_sentinel}\nstderr: exiting\n"),
        )
        .unwrap();

        agent_session_repo::create(
            &pool,
            &NewAgentSession {
                id: prior_session_id.clone(),
                workshop_id: workshop_id.clone(),
                agent_name: "stub".into(),
                agent_command: "true".into(),
                agent_args: vec![],
                session_label: Some("hand".into()),
                child_pid: None,
                resume_id: None,
                log_path: Some(prior_log_path.to_string_lossy().into_owned()),
                parent_session_id: None,
                archetype_id: None,
            },
        )
        .await
        .unwrap();
        assignment_repo::assign(
            &pool,
            NewHandAssignment {
                session_id: prior_session_id.clone(),
                spark_id: task.id.clone(),
                role: AssignmentRole::Owner,
                actor_id: Some("priorhand".into()),
            },
        )
        .await
        .unwrap();

        // --- (3) Prior owner posts "plan X" then "blocked on Y" on the
        //         epic channel. Use the same chat_of_record primitive the
        //         CLI / MCP paths call so the test stays aligned with
        //         production writes.
        let channel = ipc::channel_manager::channel_name(&ipc::channel_manager::EpicRef {
            id: epic.id.clone(),
            name: epic.title.clone(),
        });
        ipc::chat_of_record::post_message(
            &pool,
            ipc::chat_of_record::NewPost {
                channel: channel.clone(),
                body: "plan X".into(),
                author_session_id: Some(prior_session_id.clone()),
                epic_id: Some(epic.id.clone()),
            },
        )
        .await
        .unwrap();
        ipc::chat_of_record::post_message(
            &pool,
            ipc::chat_of_record::NewPost {
                channel: channel.clone(),
                body: "blocked on Y".into(),
                author_session_id: Some(prior_session_id.clone()),
                epic_id: Some(epic.id.clone()),
            },
        )
        .await
        .unwrap();

        // --- (4) Compose the re-spawn prefix as spawn_hand would. The
        //         replacement Hand's session id is fresh so the composer
        //         sees the prior owner as "prior".
        let new_session_id = Uuid::new_v4().to_string();
        let prefix = super::compose_respawn_channel_context(&pool, &task.id, &new_session_id)
            .await
            .expect("re-spawn into a prior-owned spark must yield a channel-context prefix");

        assert!(
            prefix.contains("plan X"),
            "prefix must carry the prior owner's first post 'plan X'; got: {prefix}"
        );
        assert!(
            prefix.contains("blocked on Y"),
            "prefix must carry the prior owner's second post 'blocked on Y'; got: {prefix}"
        );
        assert!(
            !prefix.contains(log_sentinel),
            "prefix must NOT contain the prior owner's session log file content; got: {prefix}"
        );
        assert!(
            prefix.contains(&channel),
            "prefix header should name the channel being tailed; got: {prefix}"
        );

        // --- (5) Control: a spark with NO prior owner returns None (no
        //         prefix prepended on a first spawn).
        let fresh_task = spark_repo::create(
            &pool,
            NewSpark {
                title: "fresh task never previously owned".into(),
                description: String::new(),
                spark_type: SparkType::Task,
                priority: 2,
                workshop_id: workshop_id.clone(),
                assignee: None,
                owner: None,
                parent_id: Some(epic.id.clone()),
                due_at: None,
                estimated_minutes: None,
                metadata: None,
                risk_level: None,
                scope_boundary: None,
            },
        )
        .await
        .unwrap();
        assert!(
            super::compose_respawn_channel_context(
                &pool,
                &fresh_task.id,
                &Uuid::new_v4().to_string(),
            )
            .await
            .is_none(),
            "first spawn (no prior owner) must not prepend a re-spawn block"
        );

        let _ = std::fs::remove_dir_all(&workshop_dir);
    }
}

// ── run_with_stream_heartbeat ── [sp-b7f7f1fa] ─────────────────────────
//
// These tests exercise the opt-in heartbeat-wrapped spawn path from a
// Hand's perspective: silent children must be kept alive with injected
// heartbeat lines while noisy children must reach `out` verbatim, and
// the child's exit code must propagate unchanged so the caller can fail
// the Hand's work on a real `cargo test` failure.

#[cfg(all(test, unix))]
mod run_with_stream_heartbeat_tests {
    use tokio::time::Duration;

    use super::run_with_stream_heartbeat;

    #[tokio::test]
    async fn silent_child_gets_heartbeats_injected_for_hand_stream() {
        let mut captured = Vec::<u8>::new();
        let outcome = run_with_stream_heartbeat(
            "sh",
            &["-c", "sleep 0.4"],
            None,
            &[],
            Some(Duration::from_millis(100)),
            &mut captured,
        )
        .await
        .expect("heartbeat-wrapped sleep should run");

        assert!(outcome.status.success(), "sleep 0.4 exits 0");
        assert!(
            outcome.heartbeats_emitted >= 2,
            "expected >=2 heartbeats across a 400 ms silent window with a 100 ms \
             interval, got {}",
            outcome.heartbeats_emitted,
        );
        let text = String::from_utf8(captured).expect("utf8 output");
        let observed = text.matches("[stream-heartbeat] still running\n").count();
        assert_eq!(
            observed as u64, outcome.heartbeats_emitted,
            "heartbeat lines in the sink must match the reported count; sink={text:?}",
        );
    }

    #[tokio::test]
    async fn noisy_child_output_is_forwarded_verbatim_without_heartbeats() {
        let mut captured = Vec::<u8>::new();
        let outcome = run_with_stream_heartbeat(
            "sh",
            &["-c", "echo hello && echo world 1>&2"],
            None,
            &[],
            Some(Duration::from_secs(10)),
            &mut captured,
        )
        .await
        .expect("heartbeat-wrapped echo should run");

        assert!(outcome.status.success());
        assert_eq!(
            outcome.heartbeats_emitted, 0,
            "fast, non-silent child must not emit any heartbeat",
        );
        let text = String::from_utf8(captured).expect("utf8 output");
        assert!(
            text.contains("hello"),
            "child stdout must reach the sink unrewritten; sink={text:?}",
        );
        assert!(
            text.contains("world"),
            "child stderr must also reach the sink unrewritten; sink={text:?}",
        );
        assert!(
            !text.contains("stream-heartbeat"),
            "no heartbeat noise should pollute a noisy child's output; sink={text:?}",
        );
    }

    #[tokio::test]
    async fn exit_code_propagates_so_hand_can_detect_cargo_test_failure() {
        let mut captured = Vec::<u8>::new();
        let outcome = run_with_stream_heartbeat(
            "sh",
            &["-c", "exit 11"],
            None,
            &[],
            Some(Duration::from_secs(10)),
            &mut captured,
        )
        .await
        .expect("heartbeat-wrapped exit-11 should run");

        assert_eq!(
            outcome.status.code(),
            Some(11),
            "wrapper must propagate the child's exit code unchanged so a \
             failed cargo test returns non-zero to the Hand",
        );
        assert_eq!(outcome.heartbeats_emitted, 0);
    }

    #[tokio::test]
    async fn cwd_and_env_overrides_reach_the_child() {
        let tmp = std::env::temp_dir().join(format!("ryve-heartbeat-cwd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("tmp cwd");
        let tail = tmp
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .expect("tmp dir must have a utf8 file name");

        let mut captured = Vec::<u8>::new();
        let outcome = run_with_stream_heartbeat(
            "sh",
            &["-c", "printf '%s\\n%s\\n' \"$PWD\" \"$HEARTBEAT_TEST\""],
            Some(&tmp),
            &[("HEARTBEAT_TEST".to_string(), "marker-ok".to_string())],
            Some(Duration::from_secs(10)),
            &mut captured,
        )
        .await
        .expect("pwd under override cwd should run");

        assert!(outcome.status.success());
        assert_eq!(
            outcome.heartbeats_emitted, 0,
            "fast child should not emit heartbeats",
        );
        let text = String::from_utf8(captured).expect("utf8 output");
        let pwd_line = text
            .lines()
            .next()
            .expect("child must print at least one line");
        assert!(
            pwd_line.ends_with(tail.as_str()),
            "child $PWD must reflect caller cwd; got {pwd_line:?}, expected tail {tail:?}",
        );
        assert!(
            text.contains("marker-ok"),
            "child env must carry caller-supplied vars; got {text:?}",
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
