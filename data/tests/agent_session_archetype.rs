// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for the `agent_sessions.archetype_id` column added in
//! migration 016. Covers spark ryve-1f9572ef's acceptance criteria:
//!
//!   * the new TEXT NULL column persists what `agent_session_repo::create`
//!     writes for the `archetype_id` field on `NewAgentSession`,
//!   * sessions inserted via the raw SQL that older code used (no
//!     archetype_id column) still load cleanly through
//!     `agent_session_repo::get` — i.e. NULL round-trips.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use data::db::open_sparks_db;
use data::sparks::agent_session_repo;
use data::sparks::types::NewAgentSession;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Workshop dir guard: remove on drop so tests leave no residue.
struct TempWorkshopDir(PathBuf);
impl Drop for TempWorkshopDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn fresh_db() -> (TempWorkshopDir, sqlx::SqlitePool) {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("ryve-arche-test-{pid}-{id}"));
    std::fs::create_dir_all(&dir).unwrap();
    let pool = open_sparks_db(&dir).await.unwrap();
    (TempWorkshopDir(dir), pool)
}

fn sample_session(id: &str, archetype_id: Option<&str>) -> NewAgentSession {
    NewAgentSession {
        id: id.to_string(),
        workshop_id: "ws-arche".to_string(),
        agent_name: "claude".to_string(),
        agent_command: "claude".to_string(),
        agent_args: Vec::new(),
        session_label: Some("hand".to_string()),
        child_pid: None,
        resume_id: None,
        log_path: None,
        parent_session_id: None,
        archetype_id: archetype_id.map(String::from),
    }
}

/// Persisting a session with a non-null `archetype_id` round-trips through
/// the repo. The spawn path in `src/hand_spawn.rs` is the real caller; this
/// test guards the data-layer half end-to-end so a future migration renaming
/// the column breaks *this* test (and nothing downstream) first.
#[tokio::test]
async fn archetype_id_is_persisted_and_readable() {
    let (_guard, pool) = fresh_db().await;

    let new = sample_session("sess-arche-1", Some("noop"));
    agent_session_repo::create(&pool, &new).await.unwrap();

    let got = agent_session_repo::get(&pool, "sess-arche-1")
        .await
        .unwrap()
        .expect("session row should exist");
    assert_eq!(got.archetype_id.as_deref(), Some("noop"));
}

/// Acceptance: "Loading an older session row with NULL archetype_id still
/// works (back-compat)." We simulate the pre-migration shape by inserting
/// directly without the new column (older code never set it) and verify
/// the typed read path loads the row without error and exposes
/// `archetype_id = None`.
#[tokio::test]
async fn null_archetype_id_round_trips_for_back_compat() {
    let (_guard, pool) = fresh_db().await;

    // 1. Via the repo with archetype_id = None — the common case for the
    //    existing Owner/Merger/Head/Investigator spawn paths.
    let repo_new = sample_session("sess-arche-null", None);
    agent_session_repo::create(&pool, &repo_new).await.unwrap();
    let repo_got = agent_session_repo::get(&pool, "sess-arche-null")
        .await
        .unwrap()
        .expect("repo-created row");
    assert!(repo_got.archetype_id.is_none());

    // 2. Via a raw INSERT that omits the archetype_id column entirely,
    //    emulating a row persisted before migration 016 landed. The
    //    typed read must still succeed with archetype_id = NULL.
    sqlx::query(
        "INSERT INTO agent_sessions \
         (id, workshop_id, agent_name, agent_command, agent_args, session_label, status, started_at) \
         VALUES ('sess-arche-legacy', 'ws-arche', 'claude', 'claude', '[]', 'hand', 'active', '2026-04-16T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("seed legacy session row");

    let legacy = agent_session_repo::get(&pool, "sess-arche-legacy")
        .await
        .unwrap()
        .expect("legacy row must still load");
    assert!(
        legacy.archetype_id.is_none(),
        "legacy row archetype_id should be NULL, got {:?}",
        legacy.archetype_id
    );
    assert_eq!(legacy.session_label.as_deref(), Some("hand"));
}
