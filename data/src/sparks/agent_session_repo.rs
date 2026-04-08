// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! CRUD for persistent agent sessions.

use sqlx::SqlitePool;

use super::error::SparksError;
use super::types::{NewAgentSession, PersistedAgentSession};

/// Create a new agent session record.
pub async fn create(pool: &SqlitePool, session: &NewAgentSession) -> Result<(), SparksError> {
    let now = chrono::Utc::now().to_rfc3339();
    let args_json = serde_json::to_string(&session.agent_args).unwrap_or_else(|_| "[]".into());

    sqlx::query(
        "INSERT INTO agent_sessions (id, workshop_id, agent_name, agent_command, agent_args, session_label, status, started_at, child_pid, resume_id, log_path, parent_session_id)
         VALUES (?, ?, ?, ?, ?, ?, 'active', ?, ?, ?, ?, ?)",
    )
    .bind(&session.id)
    .bind(&session.workshop_id)
    .bind(&session.agent_name)
    .bind(&session.agent_command)
    .bind(&args_json)
    .bind(&session.session_label)
    .bind(&now)
    .bind(session.child_pid)
    .bind(&session.resume_id)
    .bind(&session.log_path)
    .bind(&session.parent_session_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// List all sessions for a workshop, most recent first.
pub async fn list_for_workshop(
    pool: &SqlitePool,
    workshop_id: &str,
) -> Result<Vec<PersistedAgentSession>, SparksError> {
    let sessions = sqlx::query_as::<_, PersistedAgentSession>(
        "SELECT * FROM agent_sessions WHERE workshop_id = ? ORDER BY started_at DESC",
    )
    .bind(workshop_id)
    .fetch_all(pool)
    .await?;

    Ok(sessions)
}

/// Mark a session as ended.
pub async fn end_session(pool: &SqlitePool, session_id: &str) -> Result<(), SparksError> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE agent_sessions SET status = 'ended', ended_at = ? WHERE id = ?")
        .bind(&now)
        .bind(session_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Mark a session as active (when resumed).
pub async fn reactivate(pool: &SqlitePool, session_id: &str) -> Result<(), SparksError> {
    sqlx::query("UPDATE agent_sessions SET status = 'active', ended_at = NULL WHERE id = ?")
        .bind(session_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Update the resume_id for a session (e.g., after the agent reports its session ID).
pub async fn set_resume_id(
    pool: &SqlitePool,
    session_id: &str,
    resume_id: &str,
) -> Result<(), SparksError> {
    sqlx::query("UPDATE agent_sessions SET resume_id = ? WHERE id = ?")
        .bind(resume_id)
        .bind(session_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Record the detached child PID so liveness can be checked later.
pub async fn set_child_pid(
    pool: &SqlitePool,
    session_id: &str,
    child_pid: u32,
) -> Result<(), SparksError> {
    sqlx::query("UPDATE agent_sessions SET child_pid = ? WHERE id = ?")
        .bind(i64::from(child_pid))
        .bind(session_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Delete a session record.
pub async fn delete(pool: &SqlitePool, session_id: &str) -> Result<(), SparksError> {
    sqlx::query("DELETE FROM agent_sessions WHERE id = ?")
        .bind(session_id)
        .execute(pool)
        .await?;

    Ok(())
}
