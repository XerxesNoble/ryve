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
use data::sparks::types::{AssignmentRole, NewAgentSession, NewHandAssignment, Spark, SparkFilter};
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
    crew_id: Option<&str>,
    parent_session_id: Option<&str>,
) -> Result<SpawnedHand, HandSpawnError> {
    if matches!(kind, HandKind::Merger) && crew_id.is_none() {
        return Err(HandSpawnError::MergerNeedsCrew);
    }

    let ryve_dir = RyveDir::new(workshop_dir);
    ryve_dir.ensure_exists().await.map_err(HandSpawnError::Io)?;

    // 1. New session id + worktree.
    let session_id = Uuid::new_v4().to_string();
    let worktree_path = workshop::create_hand_worktree(workshop_dir, &ryve_dir, &session_id)
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
        session_label: Some(match kind {
            HandKind::Owner => "hand".to_string(),
            HandKind::Merger => "merger".to_string(),
        }),
        child_pid: None,
        resume_id: None,
        log_path: Some(log_path.to_string_lossy().into_owned()),
        parent_session_id: parent_session_id.map(|s| s.to_string()),
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
    // its child to itself.
    let mut env_vars = workshop::hand_env_vars(workshop_dir);
    env_vars.push(("RYVE_HAND_SESSION_ID".to_string(), session_id.clone()));

    // 8. Spawn detached.
    let child_pid = match launch_detached(
        &agent.command,
        &cmd_args,
        &worktree_path,
        &env_vars,
        &log_path,
    ) {
        Ok(pid) => pid,
        Err(err) => {
            let _ = assignment_repo::abandon(pool, &session_id, spark_id).await;
            let _ = agent_session_repo::end_session(pool, &session_id).await;
            return Err(err);
        }
    };

    if let Some(pid) = child_pid {
        let _ = agent_session_repo::set_child_pid(pool, &session_id, pid).await;
    }

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

#[cfg(test)]
mod tests {
    //! Integration test for spark ryve-b3ad7bd1.
    //!
    //! Spawns a Hand against a stub agent (a tiny shell script that
    //! records its arguments) inside a real temporary workshop, then
    //! asserts that the agent process actually received the user prompt
    //! containing the spark id. This is the regression: the previous
    //! implementation passed only `--system-prompt`, leaving the agent
    //! with no user message, and every Hand exited on its first turn.

    use std::time::{Duration, Instant};

    use data::sparks::spark_repo;
    use data::sparks::types::{NewSpark, SparkType};
    use uuid::Uuid;

    use super::*;
    use crate::coding_agents::{CodingAgent, ResumeStrategy};

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
        run_git(&["commit", "-q", "--allow-empty", "-m", "init"]);

        // Stub agent: prints its argv to a file. We point the file at
        // `<base>/agent-out.txt` via an env var so the test can poll it.
        let out_path = base.join("agent-out.txt");
        let stub_path = base.join("stub-agent.sh");
        std::fs::write(
            &stub_path,
            "#!/bin/sh\n\
             # Record argv so the test can verify the prompt was delivered.\n\
             printf '%s\\n' \"$@\" > \"$RYVE_TEST_AGENT_OUT\"\n",
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

    /// Acceptance criterion (3) for spark ryve-b3ad7bd1: spawning a Hand
    /// must result in a process that *receives its initial instructions*.
    /// We assert this end-to-end by reading the stub agent's recorded
    /// argv after the spawn and confirming the spark id appears in the
    /// prompt that was passed to it.
    #[tokio::test]
    async fn spawned_hand_delivers_prompt_to_agent_process() {
        let (workshop_dir, pool, out_path) = setup_workshop().await;
        let stub_path = workshop_dir.join("stub-agent.sh");

        // Create the spark in the same workshop_id the spawn helper uses.
        let workshop_id = workshop_id_for(&workshop_dir);
        let spark = spark_repo::create(
            &pool,
            NewSpark {
                title: "first turn smoke test".into(),
                description: "verify the agent receives a user prompt".into(),
                spark_type: SparkType::Bug,
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

        // Make the stub script's output path visible to the child via env.
        // `launch_detached` inherits the parent env on top of the explicit
        // pairs from `hand_env_vars`, so setting it here is sufficient.
        // SAFETY: tests run single-threaded per #[tokio::test] runtime, but
        // env mutation is process-global. This is a leaf test that doesn't
        // race with other env consumers.
        unsafe {
            std::env::set_var("RYVE_TEST_AGENT_OUT", &out_path);
        }

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
            None,
            None,
        )
        .await
        .expect("spawn_hand should succeed against the stub agent");

        assert!(spawned.child_pid.is_some(), "child pid should be reported");

        // Poll for the stub's output for up to 5 seconds. The child runs
        // detached so we cannot `wait` on it; the file appearing means the
        // process actually executed and received argv.
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

        // Cleanup — best effort, don't fail the test on filesystem hiccups.
        let _ = std::fs::remove_dir_all(&workshop_dir);
    }
}
