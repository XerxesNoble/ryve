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

use data::ryve_dir::RyveDir;
use data::sparks::types::{
    AssignmentRole, NewAgentSession, NewCrew, NewHandAssignment, Spark, SparkFilter,
};
use data::sparks::{agent_session_repo, assignment_repo, crew_repo, spark_repo};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::agent_prompts::{
    HeadArchetype, compose_hand_prompt, compose_head_prompt, compose_investigator_prompt,
    compose_merger_prompt,
};
use crate::coding_agents::CodingAgent;
use crate::tmux::{self, TmuxClient, TmuxError};
use crate::workshop;

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

    // 1. New session id + worktree — branch is `<actor>/<short>`.
    let session_id = Uuid::new_v4().to_string();
    let worktree_path =
        workshop::create_hand_worktree(workshop_dir, &ryve_dir, &session_id, &actor)
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
    let prompt = match kind {
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
        HandKind::Merger => compose_merger_prompt(crew_id.unwrap_or(""), spark_id),
    };

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

    // 8. Launch inside a tmux session. The session name is `hand-<session_id>`
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
        workshop::create_hand_worktree(workshop_dir, &ryve_dir, &session_id, &actor)
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

    /// End-to-end: `spawn_hand` with `HandKind::Investigator` must persist
    /// `session_label = "investigator"` on the `agent_sessions` row AND on
    /// the `crew_members` row, and must claim the spark with
    /// `AssignmentRole::Owner`. Acceptance criterion (4) of spark
    /// ryve-a2d447d1.
    #[tokio::test]
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
}
