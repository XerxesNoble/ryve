// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix
//
// [sp-fbf2a519] Head→Crew orchestration loop helper.
//
// A Head is a coding-agent subprocess that manages a Crew of Hands. Its
// job is NOT to edit source code — it is to decompose a goal into
// sparks, spawn Hands, poll their progress, reassign work when a Hand
// stalls, and finally spawn a Merger to integrate the result.
//
// Before this module existed, every Head archetype (Atlas in its head,
// each Build/Research/Review/Perf prompt) re-implemented that loop in
// natural language. That meant the stall threshold, the poll cadence,
// the reassignment sequence, and the "am I done?" check all lived as
// English instructions sprinkled across prompts — impossible to test,
// easy to drift.
//
// This module puts the policy in one place. A Head subprocess (and any
// Rust code that needs to do the same thing, such as the CLI
// `ryve head orchestrate` entry point) calls four primitives:
//
//   1. [`spawn_crew`]          — fan out: create the crew row, claim each
//                                child spark with its own Hand.
//   2. [`poll_crew`]           — observe: tally completed / active /
//                                stalled members using heartbeat age.
//   3. [`reassign_stalled`]    — heal: release stalled claims via
//                                [`assignment_repo::abandon`] (the data
//                                layer behind `ryve assign release`) and
//                                respawn a fresh Hand for the same spark.
//   4. [`finalize_with_merger`] — close out: once every child spark is
//                                closed `completed`, spawn the Merger
//                                Hand that opens the integration PR.
//
//! Invariant: a Head never edits source code itself. Every primitive in
//! this module either reads the workgraph, mutates the workgraph via
//! `ryve` data repos, or delegates execution to a newly-spawned Hand.
//! There is no `std::fs::write` to a project file anywhere in here by
//! design.
//
// The caller is responsible for driving the loop — whether that is an
// `ryve head orchestrate` CLI command running a `tokio::time::sleep`
// poll cadence, or a Head coding agent invoking these via the CLI one
// cycle at a time. Keeping the loop out of this module means tests can
// drive it deterministically without real sleeps.

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use data::sparks::types::{HandAssignment, NewCrew, SparkFilter};
use data::sparks::{assignment_repo, crew_repo, spark_repo};
use sqlx::SqlitePool;

use crate::coding_agents::CodingAgent;
use crate::hand_spawn::{self, HandKind, HandSpawnError, SpawnedHand};

/// Tunable thresholds for the orchestration loop. Defaults mirror the
/// values that Atlas's old-prompt loop used (60 s poll, 120 s stall) so
/// switching archetypes over to this module should be invisible.
#[derive(Debug, Clone, Copy)]
pub struct OrchestrationConfig {
    /// Poll cadence between [`poll_crew`] calls. The orchestrator
    /// module does not sleep on its own — this value is exposed so the
    /// *caller* can use it to drive its own loop or pass it to a
    /// `/loop` equivalent in a coding-agent host.
    pub poll_interval: Duration,
    /// Maximum heartbeat age before a Hand is considered stalled and
    /// eligible for reassignment. Must be longer than the agent's own
    /// heartbeat interval or healthy Hands will be killed.
    pub stall_after: Duration,
    /// Per-spark respawn cap. Prevents a genuinely broken spark from
    /// burning an unbounded number of Hands when the problem is the
    /// work, not the worker.
    pub max_respawns_per_spark: u32,
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(60),
            stall_after: Duration::from_secs(120),
            max_respawns_per_spark: 3,
        }
    }
}

/// Handle returned by [`spawn_crew`] and threaded through the rest of
/// the orchestration loop. Everything a later primitive needs to act on
/// the crew lives here so callers do not have to hang state off the
/// side.
#[derive(Debug, Clone)]
pub struct CrewHandle {
    pub crew_id: String,
    pub parent_spark_id: Option<String>,
    /// The child spark ids this Head is actively orchestrating. Tracked
    /// here — rather than re-derived from crew_members every poll — so
    /// that a user closing a spark mid-flight removes it from the
    /// orchestrator's radar immediately (poll_crew intersects this list
    /// with the current state of the workgraph).
    pub spark_ids: Vec<String>,
    /// Initially-spawned Hand session ids, one per `spark_ids` entry in
    /// the same order. [`reassign_stalled`] rewrites the relevant slot
    /// when it respawns a Hand so the handle always reflects the
    /// current owner.
    pub owners: Vec<String>,
}

/// One spark's status during a poll. Ordered by severity so a caller
/// can match on the most urgent bucket first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberState {
    /// No active owner exists for this spark. Either the previous owner
    /// released the claim (e.g. manual `ryve assign release`) or the
    /// respawn has not taken effect yet.
    Unassigned,
    /// An active owner exists but its heartbeat is older than
    /// [`OrchestrationConfig::stall_after`]. Ripe for reassignment.
    Stalled {
        session_id: String,
        last_heartbeat: Option<DateTime<Utc>>,
    },
    /// Active owner with a fresh heartbeat.
    Running { session_id: String },
    /// Spark is closed with status `completed`. Nothing more to do.
    Completed,
    /// Spark is closed with a non-completed reason (abandoned, duplicate,
    /// obsolete…). The Head should treat this as "drop from the crew"
    /// rather than "reassign".
    ClosedOther { reason: String },
}

impl MemberState {
    pub fn is_stalled(&self) -> bool {
        matches!(self, Self::Stalled { .. })
    }
    /// True iff this spark closed `completed`. Used by
    /// [`PollReport::completed`] (tests) and exposed for external code
    /// that wants a stricter "success" count.
    #[allow(dead_code)]
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed)
    }
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Completed | Self::ClosedOther { .. })
    }
}

/// Aggregate view of a crew for one poll cycle. The caller usually
/// branches on `all_done` to decide whether to call
/// [`finalize_with_merger`] next, and on `stalled` to decide whether to
/// call [`reassign_stalled`].
#[derive(Debug, Clone, Default)]
pub struct PollReport {
    pub members: Vec<(String, MemberState)>,
}

impl PollReport {
    /// Count of members that closed `completed` (not merely terminal).
    /// Exposed for callers that want to distinguish success from
    /// other terminal states like duplicates or user-cancel.
    #[allow(dead_code)]
    pub fn completed(&self) -> usize {
        self.members
            .iter()
            .filter(|(_, s)| s.is_completed())
            .count()
    }
    pub fn done(&self) -> usize {
        self.members.iter().filter(|(_, s)| s.is_done()).count()
    }
    pub fn total(&self) -> usize {
        self.members.len()
    }
    /// True iff every tracked spark reached a terminal state — either
    /// closed `completed` or closed for some other reason. This is the
    /// condition for spawning the Merger.
    pub fn all_done(&self) -> bool {
        !self.members.is_empty() && self.members.iter().all(|(_, s)| s.is_done())
    }
    /// Ids of sparks whose active Hand is stalled and should be
    /// reassigned.
    pub fn stalled_spark_ids(&self) -> Vec<String> {
        self.members
            .iter()
            .filter(|(_, s)| s.is_stalled())
            .map(|(id, _)| id.clone())
            .collect()
    }
    /// Ids of sparks with no active owner at all. These also need a
    /// fresh Hand on the next reassign pass.
    pub fn unassigned_spark_ids(&self) -> Vec<String> {
        self.members
            .iter()
            .filter(|(_, s)| matches!(s, MemberState::Unassigned))
            .map(|(id, _)| id.clone())
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OrchestrationError {
    #[error("workgraph: {0}")]
    Sparks(#[from] data::sparks::SparksError),
    #[error("spawn: {0}")]
    Spawn(#[from] HandSpawnError),
    #[error("no child sparks supplied; refusing to spawn an empty crew")]
    EmptyCrew,
}

// ─── Primitive 1: spawn_crew ──────────────────────────────────────────────

/// Create a crew row for `parent_spark_id` and spawn one Owner Hand per
/// entry in `child_spark_ids`. Returns the handle the other primitives
/// need.
///
/// `head_session_id` is the Head's own agent_sessions id, passed through
/// to `spawn_hand` as `parent_session_id` so the Head → Hand lineage is
/// visible in delegation traces. `None` is acceptable (e.g. tests) but
/// production callers should always supply it.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_crew(
    workshop_dir: &Path,
    pool: &SqlitePool,
    agent: &CodingAgent,
    workshop_id: &str,
    crew_name: &str,
    purpose: Option<&str>,
    parent_spark_id: Option<&str>,
    child_spark_ids: &[String],
    head_session_id: Option<&str>,
) -> Result<CrewHandle, OrchestrationError> {
    if child_spark_ids.is_empty() {
        return Err(OrchestrationError::EmptyCrew);
    }

    let crew = crew_repo::create(
        pool,
        NewCrew {
            name: crew_name.to_string(),
            purpose: purpose.map(|p| p.to_string()),
            workshop_id: workshop_id.to_string(),
            head_session_id: head_session_id.map(|s| s.to_string()),
            parent_spark_id: parent_spark_id.map(|s| s.to_string()),
        },
    )
    .await?;

    let mut owners = Vec::with_capacity(child_spark_ids.len());
    for spark_id in child_spark_ids {
        let spawned = hand_spawn::spawn_hand(
            workshop_dir,
            pool,
            agent,
            spark_id,
            HandKind::Owner,
            Some(&crew.id),
            head_session_id,
        )
        .await?;
        owners.push(spawned.session_id);
    }

    Ok(CrewHandle {
        crew_id: crew.id,
        parent_spark_id: parent_spark_id.map(|s| s.to_string()),
        spark_ids: child_spark_ids.to_vec(),
        owners,
    })
}

// ─── Primitive 2: poll_crew ───────────────────────────────────────────────

/// Inspect every spark the Head is orchestrating and classify it into a
/// [`MemberState`]. Pure read-path — this does not touch the workgraph.
pub async fn poll_crew(
    pool: &SqlitePool,
    crew: &CrewHandle,
    config: &OrchestrationConfig,
) -> Result<PollReport, OrchestrationError> {
    let now = Utc::now();
    let stall_after = chrono::Duration::from_std(config.stall_after).unwrap_or_else(|_| {
        // Fallback: 120 s matches OrchestrationConfig::default().
        chrono::Duration::seconds(120)
    });

    let mut members = Vec::with_capacity(crew.spark_ids.len());
    for spark_id in &crew.spark_ids {
        let spark = spark_repo::get(pool, spark_id).await?;
        let state = if spark.status == "closed" {
            match spark.closed_reason.as_deref() {
                Some("completed") => MemberState::Completed,
                Some(other) => MemberState::ClosedOther {
                    reason: other.to_string(),
                },
                None => MemberState::ClosedOther {
                    reason: "closed".to_string(),
                },
            }
        } else {
            match assignment_repo::active_for_spark(pool, spark_id).await? {
                None => MemberState::Unassigned,
                Some(owner) => classify_owner(&owner, now, stall_after),
            }
        };
        members.push((spark_id.clone(), state));
    }
    Ok(PollReport { members })
}

/// Pure classification of a single active owner — pulled out so tests
/// can drive it without touching the database.
fn classify_owner(
    owner: &HandAssignment,
    now: DateTime<Utc>,
    stall_after: chrono::Duration,
) -> MemberState {
    // Use the last heartbeat if present, otherwise fall back to
    // `assigned_at` so a Hand that never heartbeated once still
    // eventually crosses the stall threshold.
    let anchor = owner
        .last_heartbeat_at
        .as_deref()
        .or(Some(&owner.assigned_at));
    let parsed = anchor
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    match parsed {
        Some(ts) if now.signed_duration_since(ts) > stall_after => MemberState::Stalled {
            session_id: owner.session_id.clone(),
            last_heartbeat: Some(ts),
        },
        Some(_) => MemberState::Running {
            session_id: owner.session_id.clone(),
        },
        None => MemberState::Stalled {
            session_id: owner.session_id.clone(),
            last_heartbeat: None,
        },
    }
}

// ─── Primitive 3: reassign_stalled ────────────────────────────────────────

/// Release stalled (and any fully-unassigned) claims and respawn a
/// fresh Owner Hand against the same spark. Equivalent to running
/// `ryve assign release <session> <spark>` followed by `ryve hand
/// spawn <spark> --crew <crew>` for each stalled member, except it all
/// happens in-process so the Head never has to shell out.
///
/// Respects [`OrchestrationConfig::max_respawns_per_spark`]: the caller
/// passes in `respawn_counts` (a parallel map keyed by spark id) which
/// is incremented in-place. A spark that hits the cap is skipped and
/// surfaced in the returned [`ReassignReport`] so the Head can post a
/// comment and (eventually) escalate.
#[allow(clippy::too_many_arguments)]
pub async fn reassign_stalled(
    workshop_dir: &Path,
    pool: &SqlitePool,
    agent: &CodingAgent,
    crew: &mut CrewHandle,
    report: &PollReport,
    config: &OrchestrationConfig,
    respawn_counts: &mut std::collections::HashMap<String, u32>,
    head_session_id: Option<&str>,
) -> Result<ReassignReport, OrchestrationError> {
    let mut out = ReassignReport::default();

    // Reassign both Stalled (needs release first) and Unassigned
    // (nothing to release, just respawn). Treating the two paths the
    // same way keeps the Head loop uniform.
    for (spark_id, state) in &report.members {
        let needs_release_of: Option<String> = match state {
            MemberState::Stalled { session_id, .. } => Some(session_id.clone()),
            MemberState::Unassigned => None,
            _ => continue,
        };

        let count = respawn_counts.entry(spark_id.clone()).or_insert(0);
        if *count >= config.max_respawns_per_spark {
            out.capped.push(spark_id.clone());
            continue;
        }

        if let Some(session_id) = &needs_release_of {
            // Fire-and-forget: abandon is idempotent enough for our
            // purposes (it only updates rows where status='active').
            assignment_repo::abandon(pool, session_id, spark_id).await?;
            out.released.push((spark_id.clone(), session_id.clone()));
        }

        let spawned = hand_spawn::spawn_hand(
            workshop_dir,
            pool,
            agent,
            spark_id,
            HandKind::Owner,
            Some(&crew.crew_id),
            head_session_id,
        )
        .await?;

        // Keep CrewHandle::owners in sync with the current owner so
        // future polls / logging reflect the respawn.
        if let Some(pos) = crew.spark_ids.iter().position(|s| s == spark_id)
            && pos < crew.owners.len()
        {
            crew.owners[pos] = spawned.session_id.clone();
        }

        *count += 1;
        out.respawned.push(spawned);
    }
    Ok(out)
}

/// Result of a single [`reassign_stalled`] pass.
#[derive(Debug, Default)]
pub struct ReassignReport {
    /// (spark_id, released_session_id) pairs.
    pub released: Vec<(String, String)>,
    /// Freshly spawned replacement Hands.
    pub respawned: Vec<SpawnedHand>,
    /// Sparks the orchestrator declined to respawn because the
    /// per-spark respawn cap was reached. Caller should surface these
    /// (post a comment, escalate, etc.).
    pub capped: Vec<String>,
}

// ─── Primitive 4: finalize_with_merger ────────────────────────────────────

/// Spawn the Merger Hand that will open the crew's integration PR.
///
/// The caller must have already verified [`PollReport::all_done`] — this
/// primitive does not re-check, because a typical Head will create the
/// merge spark *between* the poll and the finalize call, and we don't
/// want to require the caller to rebuild a PollReport that includes the
/// newly-created merge spark.
pub async fn finalize_with_merger(
    workshop_dir: &Path,
    pool: &SqlitePool,
    agent: &CodingAgent,
    crew: &CrewHandle,
    merge_spark_id: &str,
    head_session_id: Option<&str>,
) -> Result<SpawnedHand, OrchestrationError> {
    let spawned = hand_spawn::spawn_hand(
        workshop_dir,
        pool,
        agent,
        merge_spark_id,
        HandKind::Merger,
        Some(&crew.crew_id),
        head_session_id,
    )
    .await?;

    // Flip the crew into the merging phase so the UI and any sibling
    // Heads can see that finalization has started.
    let _ = crew_repo::set_status(pool, &crew.crew_id, "merging").await;

    Ok(spawned)
}

// ─── Utility: filter spark ids that are still worth orchestrating ────────

/// Remove spark ids from `crew.spark_ids` that the workgraph has since
/// marked closed for a non-completed reason (duplicates, obsolete,
/// user-cancelled). Call this at the top of every poll cycle so the
/// orchestrator stops wasting respawns on dropped work. Returns the
/// number of sparks pruned.
pub async fn drop_closed_siblings(
    pool: &SqlitePool,
    crew: &mut CrewHandle,
) -> Result<usize, OrchestrationError> {
    let filter = SparkFilter::default();
    let all = spark_repo::list(pool, filter).await?;
    let before = crew.spark_ids.len();
    let mut kept_sparks = Vec::with_capacity(before);
    let mut kept_owners = Vec::with_capacity(before);
    for (idx, id) in crew.spark_ids.iter().enumerate() {
        let Some(sp) = all.iter().find(|s| &s.id == id) else {
            // Unknown spark — keep it so poll_crew surfaces the error
            // next cycle rather than silently dropping work.
            kept_sparks.push(id.clone());
            if idx < crew.owners.len() {
                kept_owners.push(crew.owners[idx].clone());
            }
            continue;
        };
        let dropped =
            sp.status == "closed" && !matches!(sp.closed_reason.as_deref(), Some("completed"));
        if !dropped {
            kept_sparks.push(id.clone());
            if idx < crew.owners.len() {
                kept_owners.push(crew.owners[idx].clone());
            }
        }
    }
    let removed = before - kept_sparks.len();
    crew.spark_ids = kept_sparks;
    crew.owners = kept_owners;
    Ok(removed)
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use chrono::Duration as ChronoDuration;

    use super::*;

    fn make_assignment(
        session_id: &str,
        spark_id: &str,
        heartbeat: Option<&str>,
        assigned_at: &str,
    ) -> HandAssignment {
        HandAssignment {
            id: 1,
            session_id: session_id.to_string(),
            spark_id: spark_id.to_string(),
            status: "active".to_string(),
            role: "owner".to_string(),
            assigned_at: assigned_at.to_string(),
            last_heartbeat_at: heartbeat.map(|s| s.to_string()),
            lease_expires_at: None,
            completed_at: None,
            handoff_to: None,
            handoff_reason: None,
        }
    }

    #[test]
    fn classify_owner_running_when_heartbeat_is_fresh() {
        let now = DateTime::parse_from_rfc3339("2026-01-01T00:02:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let owner = make_assignment(
            "sess-a",
            "sp-1",
            Some("2026-01-01T00:01:30Z"),
            "2026-01-01T00:00:00Z",
        );
        let state = classify_owner(&owner, now, ChronoDuration::seconds(120));
        assert!(matches!(state, MemberState::Running { .. }));
    }

    #[test]
    fn classify_owner_stalled_when_heartbeat_exceeds_threshold() {
        let now = DateTime::parse_from_rfc3339("2026-01-01T00:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let owner = make_assignment(
            "sess-a",
            "sp-1",
            Some("2026-01-01T00:01:30Z"),
            "2026-01-01T00:00:00Z",
        );
        // Threshold 120 s; gap is 3.5 min → stalled.
        let state = classify_owner(&owner, now, ChronoDuration::seconds(120));
        match state {
            MemberState::Stalled { session_id, .. } => assert_eq!(session_id, "sess-a"),
            other => panic!("expected Stalled, got {other:?}"),
        }
    }

    #[test]
    fn classify_owner_falls_back_to_assigned_at_when_no_heartbeat() {
        let now = DateTime::parse_from_rfc3339("2026-01-01T00:10:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let owner = make_assignment("sess-a", "sp-1", None, "2026-01-01T00:00:00Z");
        let state = classify_owner(&owner, now, ChronoDuration::seconds(120));
        // 10 min > 120 s → stalled even without a heartbeat row.
        assert!(matches!(state, MemberState::Stalled { .. }));
    }

    #[test]
    fn classify_owner_with_unparseable_timestamp_is_stalled() {
        let now = Utc::now();
        let owner = make_assignment("sess-a", "sp-1", Some("not-a-date"), "also-bad");
        let state = classify_owner(&owner, now, ChronoDuration::seconds(120));
        // Unparseable timestamp → treat as stalled (safer than pretending it's running).
        assert!(matches!(state, MemberState::Stalled { .. }));
    }

    #[test]
    fn poll_report_all_done_requires_nonempty_and_all_terminal() {
        let empty = PollReport::default();
        assert!(!empty.all_done(), "empty report is not 'done'");

        let mut partial = PollReport::default();
        partial
            .members
            .push(("sp-1".into(), MemberState::Completed));
        partial.members.push((
            "sp-2".into(),
            MemberState::Running {
                session_id: "sess".into(),
            },
        ));
        assert!(!partial.all_done());
        assert_eq!(partial.completed(), 1);
        assert_eq!(partial.done(), 1);

        let mut all = PollReport::default();
        all.members.push(("sp-1".into(), MemberState::Completed));
        all.members.push((
            "sp-2".into(),
            MemberState::ClosedOther {
                reason: "obsolete".into(),
            },
        ));
        assert!(all.all_done());
        // Completed is the stricter bucket; ClosedOther counts in done() only.
        assert_eq!(all.completed(), 1);
        assert_eq!(all.done(), 2);
    }

    #[test]
    fn poll_report_surfaces_stalled_and_unassigned_ids() {
        let mut r = PollReport::default();
        r.members.push((
            "sp-stalled".into(),
            MemberState::Stalled {
                session_id: "old".into(),
                last_heartbeat: None,
            },
        ));
        r.members
            .push(("sp-unowned".into(), MemberState::Unassigned));
        r.members.push((
            "sp-running".into(),
            MemberState::Running {
                session_id: "fresh".into(),
            },
        ));
        assert_eq!(r.stalled_spark_ids(), vec!["sp-stalled"]);
        assert_eq!(r.unassigned_spark_ids(), vec!["sp-unowned"]);
    }

    #[test]
    fn orchestration_config_defaults_are_sane() {
        let cfg = OrchestrationConfig::default();
        assert_eq!(cfg.poll_interval, Duration::from_secs(60));
        assert_eq!(cfg.stall_after, Duration::from_secs(120));
        assert!(cfg.max_respawns_per_spark >= 1);
        // Stall window must be *longer* than the poll cadence, otherwise
        // a perfectly healthy Hand that heartbeats once per poll cycle
        // could be killed on the next tick.
        assert!(cfg.stall_after > cfg.poll_interval);
    }

    #[test]
    fn member_state_helpers_cover_terminal_variants() {
        assert!(MemberState::Completed.is_done());
        assert!(
            MemberState::ClosedOther {
                reason: "dup".into()
            }
            .is_done()
        );
        assert!(
            !MemberState::Running {
                session_id: "s".into()
            }
            .is_done()
        );
        assert!(!MemberState::Unassigned.is_done());
        assert!(
            MemberState::Stalled {
                session_id: "s".into(),
                last_heartbeat: None
            }
            .is_stalled()
        );
    }
}
