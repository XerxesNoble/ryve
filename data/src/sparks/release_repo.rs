// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Typed CRUD for the Workgraph `releases` / `release_epics` tables.
//!
//! Spark ryve-d5032784 [sp-2a82fee7]: this is the foundation every later
//! release-planning spark depends on. Keep the surface small, typed, and
//! enforce the open-release invariant from [`ReleaseStatus::is_open`].

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::id::generate_id;
use super::types::*;
use crate::release_version;

/// Validate that `s` is a strict `MAJOR.MINOR.PATCH` semver string.
///
/// Pre-release tags (`-alpha`) and build metadata (`+build`) are rejected
/// because the rest of the release pipeline (branching, tagging, artifacts)
/// operates on strict `MAJOR.MINOR.PATCH` only.
pub fn validate_semver(s: &str) -> Result<(), SparksError> {
    let invalid = || SparksError::InvalidSemver(s.to_string());

    // Reject pre-release and build metadata outright.
    if s.contains('-') || s.contains('+') {
        return Err(invalid());
    }

    // Must be exactly three dotted numeric segments.
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return Err(invalid());
    }
    for part in parts {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return Err(invalid());
        }
        if part.len() > 1 && part.starts_with('0') {
            return Err(invalid());
        }
    }

    Ok(())
}

/// Create a new release in `planning` state.
pub async fn create(pool: &SqlitePool, new: NewRelease) -> Result<Release, SparksError> {
    validate_semver(&new.version)?;

    let id = generate_id("rel");
    let now = Utc::now().to_rfc3339();
    let acceptance_json = serde_json::to_string(&new.acceptance)
        .map_err(|e| SparksError::Serialization(e.to_string()))?;

    sqlx::query(
        "INSERT INTO releases (id, version, status, branch_name, created_at, problem, acceptance_json, notes)
         VALUES (?, ?, 'planning', ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.version)
    .bind(&new.branch_name)
    .bind(&now)
    .bind(&new.problem)
    .bind(&acceptance_json)
    .bind(&new.notes)
    .execute(pool)
    .await?;

    get(pool, &id).await
}

/// Fetch a single release by id.
pub async fn get(pool: &SqlitePool, id: &str) -> Result<Release, SparksError> {
    sqlx::query_as::<_, Release>("SELECT * FROM releases WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| SparksError::NotFound(format!("release {id}")))
}

/// List releases, optionally filtered to a set of statuses. An empty filter
/// returns everything, ordered newest-first.
pub async fn list(
    pool: &SqlitePool,
    statuses: Option<Vec<ReleaseStatus>>,
) -> Result<Vec<Release>, SparksError> {
    let mut sql = String::from("SELECT * FROM releases");
    let mut bindings: Vec<String> = Vec::new();

    if let Some(ss) = statuses.filter(|s| !s.is_empty()) {
        let placeholders = ss.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        sql.push_str(&format!(" WHERE status IN ({placeholders})"));
        for s in ss {
            bindings.push(s.as_str().to_string());
        }
    }
    sql.push_str(" ORDER BY created_at DESC");

    let mut q = sqlx::query_as::<_, Release>(&sql);
    for b in &bindings {
        q = q.bind(b);
    }
    Ok(q.fetch_all(pool).await?)
}

/// Update mutable fields on an existing release. Only fields that are `Some`
/// in `patch` are written; `None` fields are left unchanged.
pub async fn update(
    pool: &SqlitePool,
    id: &str,
    patch: UpdateRelease,
) -> Result<Release, SparksError> {
    let Release {
        version: existing_version,
        problem: existing_problem,
        notes: existing_notes,
        ..
    } = get(pool, id).await?;

    let version = match patch.version {
        Some(v) => {
            release_version::parse(&v).map_err(|_| SparksError::InvalidSemver(v.clone()))?;
            v
        }
        None => existing_version,
    };

    let problem = match patch.problem {
        Some(opt) => opt,
        None => existing_problem,
    };
    let notes = match patch.notes {
        Some(opt) => opt,
        None => existing_notes,
    };

    sqlx::query("UPDATE releases SET version = ?, problem = ?, notes = ? WHERE id = ?")
        .bind(&version)
        .bind(&problem)
        .bind(&notes)
        .bind(id)
        .execute(pool)
        .await?;

    get(pool, id).await
}

/// Add an epic spark to a release.
///
/// Returns [`SparksError::EpicAlreadyInOpenRelease`] if the spark is already
/// a member of some *other* open release (`planning|in_progress|ready`).
pub async fn add_epic(
    pool: &SqlitePool,
    release_id: &str,
    spark_id: &str,
) -> Result<(), SparksError> {
    // Confirm the release exists up front so callers get a typed NotFound
    // instead of a foreign-key failure from the trigger-backed table.
    let _ = get(pool, release_id).await?;

    let now = Utc::now().to_rfc3339();
    let res =
        sqlx::query("INSERT INTO release_epics (release_id, spark_id, added_at) VALUES (?, ?, ?)")
            .bind(release_id)
            .bind(spark_id)
            .bind(&now)
            .execute(pool)
            .await;

    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(map_epic_conflict(e, spark_id)),
    }
}

/// Remove an epic from a release. No-op if the link does not exist.
pub async fn remove_epic(
    pool: &SqlitePool,
    release_id: &str,
    spark_id: &str,
) -> Result<(), SparksError> {
    sqlx::query("DELETE FROM release_epics WHERE release_id = ? AND spark_id = ?")
        .bind(release_id)
        .bind(spark_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Transition a release to a new status. Validates that the release exists
/// and surfaces the reopen-conflict trigger as [`SparksError::EpicAlreadyInOpenRelease`].
pub async fn set_status(
    pool: &SqlitePool,
    release_id: &str,
    status: ReleaseStatus,
) -> Result<Release, SparksError> {
    let existing = get(pool, release_id).await?;
    let now = Utc::now().to_rfc3339();

    // Stamp cut_at the first time the release reaches `cut`.
    let new_cut_at = if matches!(status, ReleaseStatus::Cut) && existing.cut_at.is_none() {
        Some(now.clone())
    } else {
        existing.cut_at.clone()
    };

    let res = sqlx::query("UPDATE releases SET status = ?, cut_at = ? WHERE id = ?")
        .bind(status.as_str())
        .bind(&new_cut_at)
        .bind(release_id)
        .execute(pool)
        .await;

    if let Err(e) = res {
        // The reopen trigger does not know which spark conflicted, so use
        // a synthetic marker. Callers can still distinguish the variant.
        return Err(map_epic_conflict(e, "<reopen>"));
    }

    get(pool, release_id).await
}

/// List the spark ids that are members of `release_id`, in the order they
/// were added.
pub async fn list_member_epics(
    pool: &SqlitePool,
    release_id: &str,
) -> Result<Vec<String>, SparksError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT spark_id FROM release_epics WHERE release_id = ? ORDER BY added_at ASC",
    )
    .bind(release_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(s,)| s).collect())
}

/// Record the tag name and artifact path on a release. Used by the close flow
/// after tagging + building so the release row carries pointers to both.
pub async fn record_close_metadata(
    pool: &SqlitePool,
    release_id: &str,
    tag: &str,
    artifact_path: &str,
) -> Result<(), SparksError> {
    sqlx::query("UPDATE releases SET tag = ?, artifact_path = ? WHERE id = ?")
        .bind(tag)
        .bind(artifact_path)
        .bind(release_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Translate a raw sqlx error into a typed epic-conflict error when the
/// message matches one of our triggers' ABORT strings.
fn map_epic_conflict(err: sqlx::Error, spark_id: &str) -> SparksError {
    let msg = err.to_string();
    if msg.contains("release_epic conflict") || msg.contains("release_status conflict") {
        SparksError::EpicAlreadyInOpenRelease {
            spark_id: spark_id.to_string(),
        }
    } else {
        SparksError::Database(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_semver_accepts_strict_triples() {
        for v in ["0.0.0", "1.2.3", "10.20.30"] {
            assert!(validate_semver(v).is_ok(), "expected {v} to parse");
        }
    }

    #[test]
    fn validate_semver_rejects_bad_input() {
        for v in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1.2.a",
            "01.2.3",
            "1.2.3-",
            "1.2.3+",
            "1.2.3-alpha",
            "1.0.0-alpha.1",
            "1.0.0+build",
            "1.0.0-rc.1+build.2",
        ] {
            assert!(validate_semver(v).is_err(), "expected {v} to be rejected");
        }
    }
}
