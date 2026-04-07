// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

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

use std::path::{Path, PathBuf};

use data::ryve_dir::RyveDir;
use data::sparks::types::{
    AssignmentRole, NewAgentSession, NewHandAssignment, Spark, SparkFilter,
};
use data::sparks::{agent_session_repo, assignment_repo, crew_repo, spark_repo};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::agent_prompts::{compose_hand_prompt, compose_merger_prompt};
use crate::coding_agents::CodingAgent;
use crate::workshop;

/// What kind of Hand we are spawning. Determines which initial prompt is
/// composed and which `AssignmentRole` is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandKind {
    /// Standard owner-of-the-spark Hand.
    Owner,
    /// The crew's integrator. Requires `crew_id` to be set.
    Merger,
}

impl HandKind {
    fn role(self) -> AssignmentRole {
        match self {
            Self::Owner => AssignmentRole::Owner,
            Self::Merger => AssignmentRole::Merger,
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
    #[error("agent command not found on PATH: {0}")]
    AgentMissing(String),
}

/// Spawn a Hand programmatically. Used by `ryve hand spawn` and by tests.
///
/// `workshop_dir` is the workshop root (where `.ryve/` lives).
/// `agent` is the coding agent definition (claude / codex / aider / opencode
/// or any custom agent registered in the future).
/// `kind` decides Owner vs Merger.
/// `crew_id` attaches the new Hand to a Crew via `crew_repo::add_member`.
pub async fn spawn_hand(
    workshop_dir: &Path,
    pool: &SqlitePool,
    agent: &CodingAgent,
    spark_id: &str,
    kind: HandKind,
    crew_id: Option<&str>,
) -> Result<SpawnedHand, HandSpawnError> {
    if matches!(kind, HandKind::Merger) && crew_id.is_none() {
        return Err(HandSpawnError::MergerNeedsCrew);
    }

    let ryve_dir = RyveDir::new(workshop_dir);
    ryve_dir.ensure_exists().await.map_err(HandSpawnError::Io)?;

    // 1. New session id + worktree.
    let session_id = Uuid::new_v4().to_string();
    let worktree_path = workshop::create_hand_worktree(workshop_dir, &ryve_dir, &session_id)
        .map_err(HandSpawnError::Worktree)?;

    // 2. Persist the agent session.
    let new_session = NewAgentSession {
        id: session_id.clone(),
        workshop_id: workshop_id_for(workshop_dir),
        agent_name: agent.display_name.clone(),
        agent_command: agent.command.clone(),
        agent_args: agent.args.clone(),
        session_label: Some(match kind {
            HandKind::Owner => "hand".to_string(),
            HandKind::Merger => "merger".to_string(),
        }),
        resume_id: None,
    };
    agent_session_repo::create(pool, &new_session).await?;

    // 3. Claim the spark.
    let assignment = NewHandAssignment {
        session_id: session_id.clone(),
        spark_id: spark_id.to_string(),
        role: kind.role(),
    };
    assignment_repo::assign(pool, assignment).await?;

    // 4. Add to crew if requested.
    if let Some(cid) = crew_id {
        let role_label = match kind {
            HandKind::Owner => "hand",
            HandKind::Merger => "merger",
        };
        crew_repo::add_member(pool, cid, &session_id, Some(role_label)).await?;
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
    let logs_dir = ryve_dir.root().join("logs");
    tokio::fs::create_dir_all(&logs_dir).await?;
    let log_path = logs_dir.join(format!("hand-{session_id}.log"));

    let mut cmd_args: Vec<String> = agent.args.clone();
    if let Some((flag, is_file)) = agent.system_prompt_flag() {
        cmd_args.push(flag.to_string());
        cmd_args.push(if is_file {
            prompt_path.to_string_lossy().into_owned()
        } else {
            prompt.clone()
        });
    }
    cmd_args.extend(agent.full_auto_flags());

    let env_vars = workshop::hand_env_vars(workshop_dir);

    // 8. Spawn detached.
    let child_pid = launch_detached(&agent.command, &cmd_args, &worktree_path, &env_vars, &log_path)?;

    Ok(SpawnedHand {
        session_id,
        spark_id: spark_id.to_string(),
        worktree_path,
        log_path,
        child_pid,
    })
}

/// Spawn a child process detached from the parent. stdout/stderr go to
/// `log_path`; stdin is /dev/null. On Unix we call `setsid` so closing the
/// parent terminal does not propagate SIGHUP. Returns the child PID.
fn launch_detached(
    command: &str,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    log_path: &Path,
) -> Result<Option<u32>, HandSpawnError> {
    use std::process::{Command, Stdio};

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let log_file_err = log_file.try_clone()?;

    let mut cmd = Command::new(command);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err));
    for (k, v) in env {
        cmd.env(k, v);
    }

    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        // setsid() disconnects the new process from the controlling terminal
        // so closing the Head's bench tab does not kill its Hands.
        cmd.pre_exec(|| {
            // libc::setsid returns -1 on error.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    match cmd.spawn() {
        Ok(child) => Ok(Some(child.id())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(HandSpawnError::AgentMissing(command.to_string()))
        }
        Err(e) => Err(HandSpawnError::Io(e)),
    }
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
