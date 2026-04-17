// SPDX-License-Identifier: AGPL-3.0-or-later

//! CRUD for [`Watch`] rows — Atlas's durable "recurring observation"
//! primitive.
//!
//! Spark ryve-7f5a9e5b [sp-ee3f5c74]: this module is the persistence
//! foundation for the watch system. Every downstream sibling (scheduler,
//! firing loop, UI, wiring) reads and writes through this repo, so the
//! surface is kept deliberately small and typed. See migration
//! `017_watches.sql` for the schema and invariants enforced at the DB layer.

use chrono::Utc;
use sqlx::SqlitePool;

use super::error::SparksError;
use super::id::generate_id;
use super::types::*;

/// Create a new active watch.
///
/// Returns [`SparksError::DuplicateWatch`] if the
/// `(target_spark_id, intent_label)` pair already has a non-cancelled row.
pub async fn create(pool: &SqlitePool, new: NewWatch) -> Result<Watch, SparksError> {
    let id = generate_id("watch");
    let now = Utc::now().to_rfc3339();
    let cadence = new.cadence.to_storage();
    let stop_condition = new.stop_condition.as_ref().and_then(|s| s.to_storage());

    let res = sqlx::query(
        "INSERT INTO watches (
             id, target_spark_id, cadence, stop_condition, intent_label,
             status, last_fired_at, next_fire_at, created_at, updated_at, created_by
         ) VALUES (?, ?, ?, ?, ?, 'active', NULL, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.target_spark_id)
    .bind(&cadence)
    .bind(&stop_condition)
    .bind(&new.intent_label)
    .bind(&new.next_fire_at)
    .bind(&now)
    .bind(&now)
    .bind(&new.created_by)
    .execute(pool)
    .await;

    match res {
        Ok(_) => get(pool, &id).await,
        Err(e) => Err(map_duplicate(e, &new.target_spark_id, &new.intent_label)),
    }
}

/// Fetch a single watch by id.
pub async fn get(pool: &SqlitePool, id: &str) -> Result<Watch, SparksError> {
    sqlx::query_as::<_, Watch>("SELECT * FROM watches WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| SparksError::NotFound(format!("watch {id}")))
}

/// List watches, filtered by status and/or target spark. An empty filter
/// returns everything newest-first.
pub async fn list(pool: &SqlitePool, filter: WatchFilter) -> Result<Vec<Watch>, SparksError> {
    let mut sql = String::from("SELECT * FROM watches");
    let mut clauses: Vec<&str> = Vec::new();
    if filter.status.is_some() {
        clauses.push("status = ?");
    }
    if filter.target_spark_id.is_some() {
        clauses.push("target_spark_id = ?");
    }
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(" ORDER BY created_at DESC");

    let mut q = sqlx::query_as::<_, Watch>(&sql);
    if let Some(status) = filter.status {
        q = q.bind(status.as_str().to_string());
    }
    if let Some(target) = filter.target_spark_id {
        q = q.bind(target);
    }
    Ok(q.fetch_all(pool).await?)
}

/// Soft-cancel a watch by setting `status = 'cancelled'`. The row itself
/// is preserved so audit history (and any downstream event links) remain
/// intact. No-op if the watch is already cancelled.
pub async fn cancel(pool: &SqlitePool, id: &str) -> Result<Watch, SparksError> {
    let _ = get(pool, id).await?;
    let now = Utc::now().to_rfc3339();
    sqlx::query("UPDATE watches SET status = 'cancelled', updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(id)
        .execute(pool)
        .await?;
    get(pool, id).await
}

/// Atomically cancel an existing watch and create a replacement. Both
/// operations run inside a single transaction, so a caller will never
/// observe a gap where neither the old nor the new watch exists.
///
/// Useful for re-tuning cadence or stop conditions on a live watch: the
/// partial unique index excludes cancelled rows, so the new insert is
/// unaffected by the old one's `(target, intent)` tuple.
pub async fn replace(
    pool: &SqlitePool,
    existing_id: &str,
    new: NewWatch,
) -> Result<Watch, SparksError> {
    let id = generate_id("watch");
    let now = Utc::now().to_rfc3339();
    let cadence = new.cadence.to_storage();
    let stop_condition = new.stop_condition.as_ref().and_then(|s| s.to_storage());

    let mut tx = pool.begin().await?;

    // Confirm the old watch exists — surface a typed NotFound instead of
    // silently creating a standalone row if the caller passed a bad id.
    let existing: Option<Watch> = sqlx::query_as::<_, Watch>("SELECT * FROM watches WHERE id = ?")
        .bind(existing_id)
        .fetch_optional(&mut *tx)
        .await?;
    if existing.is_none() {
        return Err(SparksError::NotFound(format!("watch {existing_id}")));
    }

    sqlx::query("UPDATE watches SET status = 'cancelled', updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(existing_id)
        .execute(&mut *tx)
        .await?;

    let insert = sqlx::query(
        "INSERT INTO watches (
             id, target_spark_id, cadence, stop_condition, intent_label,
             status, last_fired_at, next_fire_at, created_at, updated_at, created_by
         ) VALUES (?, ?, ?, ?, ?, 'active', NULL, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.target_spark_id)
    .bind(&cadence)
    .bind(&stop_condition)
    .bind(&new.intent_label)
    .bind(&new.next_fire_at)
    .bind(&now)
    .bind(&now)
    .bind(&new.created_by)
    .execute(&mut *tx)
    .await;

    if let Err(e) = insert {
        return Err(map_duplicate(e, &new.target_spark_id, &new.intent_label));
    }

    tx.commit().await?;

    get(pool, &id).await
}

/// Record that a watch just fired: advances `last_fired_at` to `fired_at`
/// and `next_fire_at` to the next scheduled instant (computed by the
/// caller — the repo is intentionally agnostic to the cadence math).
pub async fn mark_fired(
    pool: &SqlitePool,
    id: &str,
    fired_at: &str,
    next_fire_at: &str,
) -> Result<Watch, SparksError> {
    let _ = get(pool, id).await?;
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE watches
            SET last_fired_at = ?, next_fire_at = ?, updated_at = ?
          WHERE id = ?",
    )
    .bind(fired_at)
    .bind(next_fire_at)
    .bind(&now)
    .bind(id)
    .execute(pool)
    .await?;
    get(pool, id).await
}

/// Transactional variant of [`mark_fired`]. The scheduler calls this
/// inside the same transaction as the `WatchFired` outbox insert so the
/// event row and the advanced `next_fire_at` commit together — a crash
/// between the two is impossible. Spark ryve-6ab1980c [sp-ee3f5c74].
pub async fn mark_fired_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
    fired_at: &str,
    next_fire_at: &str,
) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE watches
            SET last_fired_at = ?, next_fire_at = ?, updated_at = ?
          WHERE id = ?",
    )
    .bind(fired_at)
    .bind(next_fire_at)
    .bind(&now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Transactional stop-condition transition: mark a watch `completed` and
/// stamp `last_fired_at` in the same transaction as the final
/// `WatchFired` outbox insert. Used by the scheduler when the watch's
/// stop condition is satisfied on this tick; future ticks will not see
/// it (`due_at` filters `status = 'active'`). Spark ryve-6ab1980c.
pub async fn mark_completed_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
    fired_at: &str,
) -> Result<(), SparksError> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE watches
            SET status = 'completed', last_fired_at = ?, updated_at = ?
          WHERE id = ?",
    )
    .bind(fired_at)
    .bind(&now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Return every active watch whose `next_fire_at <= now`. Used by the
/// scheduler tick to find work that needs firing.
pub async fn due_at(pool: &SqlitePool, now: &str) -> Result<Vec<Watch>, SparksError> {
    Ok(sqlx::query_as::<_, Watch>(
        "SELECT * FROM watches
          WHERE status = 'active' AND next_fire_at <= ?
          ORDER BY next_fire_at ASC",
    )
    .bind(now)
    .fetch_all(pool)
    .await?)
}

/// Translate a raw sqlx error into [`SparksError::DuplicateWatch`] when it
/// matches the partial unique index on `(target_spark_id, intent_label)`.
fn map_duplicate(err: sqlx::Error, target_spark_id: &str, intent_label: &str) -> SparksError {
    let msg = err.to_string();
    // sqlx surfaces UNIQUE-constraint failures with the offending index
    // name in the message, e.g. "UNIQUE constraint failed:
    // watches.target_spark_id, watches.intent_label". Match on the column
    // list so we do not mis-classify future unique indexes (if any) on the
    // same table.
    if msg.contains("UNIQUE constraint failed")
        && msg.contains("watches.target_spark_id")
        && msg.contains("watches.intent_label")
    {
        SparksError::DuplicateWatch {
            target_spark_id: target_spark_id.to_string(),
            intent_label: intent_label.to_string(),
        }
    } else {
        SparksError::Database(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_new(target: &str, intent: &str, next_fire_at: &str) -> NewWatch {
        NewWatch {
            target_spark_id: target.to_string(),
            cadence: WatchCadence::Interval { secs: 60 },
            stop_condition: None,
            intent_label: intent.to_string(),
            next_fire_at: next_fire_at.to_string(),
            created_by: Some("atlas".to_string()),
        }
    }

    #[sqlx::test]
    async fn create_persists_all_fields(pool: SqlitePool) {
        let w = create(
            &pool,
            NewWatch {
                target_spark_id: "ryve-target".to_string(),
                cadence: WatchCadence::Cron {
                    expr: "*/5 * * * *".to_string(),
                },
                stop_condition: Some(WatchStopCondition::UntilSparkStatus {
                    spark_id: "ryve-target".to_string(),
                    status: "closed".to_string(),
                }),
                intent_label: "poll-status".to_string(),
                next_fire_at: "2026-04-17T07:00:00+00:00".to_string(),
                created_by: Some("atlas".to_string()),
            },
        )
        .await
        .unwrap();

        assert!(w.id.starts_with("watch-"));
        assert_eq!(w.target_spark_id, "ryve-target");
        assert_eq!(w.intent_label, "poll-status");
        assert_eq!(w.status, "active");
        assert_eq!(w.last_fired_at, None);
        assert_eq!(w.created_by.as_deref(), Some("atlas"));
        assert_eq!(
            w.parsed_cadence(),
            Some(WatchCadence::Cron {
                expr: "*/5 * * * *".to_string()
            })
        );
        assert_eq!(
            w.parsed_stop_condition(),
            Some(WatchStopCondition::UntilSparkStatus {
                spark_id: "ryve-target".to_string(),
                status: "closed".to_string(),
            })
        );
        assert_eq!(w.parsed_status(), Some(WatchStatus::Active));
    }

    #[sqlx::test]
    async fn create_rejects_duplicate_target_intent(pool: SqlitePool) {
        create(
            &pool,
            sample_new("ryve-dup", "intent-a", "2026-04-17T07:00:00+00:00"),
        )
        .await
        .unwrap();

        let err = create(
            &pool,
            sample_new("ryve-dup", "intent-a", "2026-04-17T07:05:00+00:00"),
        )
        .await
        .unwrap_err();

        match err {
            SparksError::DuplicateWatch {
                target_spark_id,
                intent_label,
            } => {
                assert_eq!(target_spark_id, "ryve-dup");
                assert_eq!(intent_label, "intent-a");
            }
            other => panic!("expected DuplicateWatch, got {other:?}"),
        }
    }

    #[sqlx::test]
    async fn create_allows_same_target_different_intent(pool: SqlitePool) {
        create(
            &pool,
            sample_new("ryve-same", "intent-a", "2026-04-17T07:00:00+00:00"),
        )
        .await
        .unwrap();
        create(
            &pool,
            sample_new("ryve-same", "intent-b", "2026-04-17T07:05:00+00:00"),
        )
        .await
        .unwrap();

        let rows = list(
            &pool,
            WatchFilter {
                target_spark_id: Some("ryve-same".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[sqlx::test]
    async fn cancel_marks_status_cancelled(pool: SqlitePool) {
        let w = create(
            &pool,
            sample_new("ryve-cancel", "intent", "2026-04-17T07:00:00+00:00"),
        )
        .await
        .unwrap();

        let cancelled = cancel(&pool, &w.id).await.unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert_eq!(cancelled.parsed_status(), Some(WatchStatus::Cancelled));

        // Row is preserved, not deleted.
        let fetched = get(&pool, &w.id).await.unwrap();
        assert_eq!(fetched.status, "cancelled");
    }

    #[sqlx::test]
    async fn cancel_frees_unique_slot_for_new_watch(pool: SqlitePool) {
        let w = create(
            &pool,
            sample_new("ryve-reopen", "intent", "2026-04-17T07:00:00+00:00"),
        )
        .await
        .unwrap();
        cancel(&pool, &w.id).await.unwrap();

        // Once the old row is cancelled the partial index no longer covers
        // it, so a new active watch on the same (target, intent) succeeds.
        create(
            &pool,
            sample_new("ryve-reopen", "intent", "2026-04-17T07:05:00+00:00"),
        )
        .await
        .unwrap();
    }

    #[sqlx::test]
    async fn replace_is_atomic(pool: SqlitePool) {
        let original = create(
            &pool,
            sample_new("ryve-replace", "intent", "2026-04-17T07:00:00+00:00"),
        )
        .await
        .unwrap();

        let next = replace(
            &pool,
            &original.id,
            NewWatch {
                cadence: WatchCadence::Interval { secs: 300 },
                ..sample_new("ryve-replace", "intent", "2026-04-17T08:00:00+00:00")
            },
        )
        .await
        .unwrap();

        assert_ne!(next.id, original.id);
        assert_eq!(next.status, "active");
        assert_eq!(
            next.parsed_cadence(),
            Some(WatchCadence::Interval { secs: 300 })
        );

        // Old watch must now be cancelled; count of active rows for this
        // (target, intent) must be exactly one.
        let old = get(&pool, &original.id).await.unwrap();
        assert_eq!(old.status, "cancelled");

        let active = list(
            &pool,
            WatchFilter {
                status: Some(WatchStatus::Active),
                target_spark_id: Some("ryve-replace".to_string()),
            },
        )
        .await
        .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, next.id);
    }

    #[sqlx::test]
    async fn replace_rejects_unknown_existing_id(pool: SqlitePool) {
        let err = replace(
            &pool,
            "watch-nonexistent",
            sample_new("ryve-x", "intent", "2026-04-17T07:00:00+00:00"),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, SparksError::NotFound(_)));
    }

    #[sqlx::test]
    async fn mark_fired_updates_timestamps(pool: SqlitePool) {
        let w = create(
            &pool,
            sample_new("ryve-fire", "intent", "2026-04-17T07:00:00+00:00"),
        )
        .await
        .unwrap();

        let fired = mark_fired(
            &pool,
            &w.id,
            "2026-04-17T07:00:05+00:00",
            "2026-04-17T07:01:05+00:00",
        )
        .await
        .unwrap();

        assert_eq!(
            fired.last_fired_at.as_deref(),
            Some("2026-04-17T07:00:05+00:00")
        );
        assert_eq!(fired.next_fire_at, "2026-04-17T07:01:05+00:00");
        // updated_at must advance past created_at.
        assert_ne!(fired.updated_at, w.updated_at);
    }

    #[sqlx::test]
    async fn due_at_filters_correctly(pool: SqlitePool) {
        // Past-due: should be returned.
        let past = create(
            &pool,
            sample_new("ryve-due-a", "intent", "2026-04-17T06:00:00+00:00"),
        )
        .await
        .unwrap();
        // Exactly at the tick — boundary is inclusive.
        let boundary = create(
            &pool,
            sample_new("ryve-due-b", "intent", "2026-04-17T07:00:00+00:00"),
        )
        .await
        .unwrap();
        // Future: should be excluded.
        let _future = create(
            &pool,
            sample_new("ryve-due-c", "intent", "2026-04-17T08:00:00+00:00"),
        )
        .await
        .unwrap();
        // Cancelled past-due: must not fire again.
        let cancelled = create(
            &pool,
            sample_new("ryve-due-d", "intent", "2026-04-17T05:00:00+00:00"),
        )
        .await
        .unwrap();
        cancel(&pool, &cancelled.id).await.unwrap();

        let rows = due_at(&pool, "2026-04-17T07:00:00+00:00").await.unwrap();
        let ids: Vec<&str> = rows.iter().map(|w| w.id.as_str()).collect();
        assert!(ids.contains(&past.id.as_str()));
        assert!(ids.contains(&boundary.id.as_str()));
        assert_eq!(ids.len(), 2);
    }

    #[sqlx::test]
    async fn list_filters_by_status_and_target(pool: SqlitePool) {
        let a = create(
            &pool,
            sample_new("ryve-list-a", "i", "2026-04-17T07:00:00+00:00"),
        )
        .await
        .unwrap();
        let _b = create(
            &pool,
            sample_new("ryve-list-b", "i", "2026-04-17T07:00:00+00:00"),
        )
        .await
        .unwrap();
        cancel(&pool, &a.id).await.unwrap();

        let only_cancelled = list(
            &pool,
            WatchFilter {
                status: Some(WatchStatus::Cancelled),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(only_cancelled.len(), 1);
        assert_eq!(only_cancelled[0].id, a.id);

        let only_b = list(
            &pool,
            WatchFilter {
                target_spark_id: Some("ryve-list-b".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(only_b.len(), 1);
        assert_eq!(only_b[0].target_spark_id, "ryve-list-b");
    }

    #[test]
    fn watch_cadence_round_trips_through_storage() {
        let interval = WatchCadence::Interval { secs: 90 };
        assert_eq!(interval.to_storage(), "interval-secs:90");
        assert_eq!(
            WatchCadence::from_storage("interval-secs:90"),
            Some(interval)
        );

        let cron = WatchCadence::Cron {
            expr: "0 * * * *".to_string(),
        };
        assert_eq!(cron.to_storage(), "cron:0 * * * *");
        assert_eq!(WatchCadence::from_storage("cron:0 * * * *"), Some(cron));

        assert_eq!(WatchCadence::from_storage("bogus"), None);
        assert_eq!(WatchCadence::from_storage("interval-secs:"), None);
        assert_eq!(WatchCadence::from_storage("cron:"), None);
    }

    #[test]
    fn watch_stop_condition_round_trips_through_storage() {
        assert_eq!(WatchStopCondition::Never.to_storage(), None);

        let ev = WatchStopCondition::UntilEventType {
            event_type: "spark.closed".to_string(),
        };
        let encoded = ev.to_storage().unwrap();
        assert_eq!(WatchStopCondition::from_storage(&encoded), Some(ev));

        let st = WatchStopCondition::UntilSparkStatus {
            spark_id: "ryve-x".to_string(),
            status: "merged".to_string(),
        };
        let encoded = st.to_storage().unwrap();
        assert_eq!(WatchStopCondition::from_storage(&encoded), Some(st));

        assert_eq!(WatchStopCondition::from_storage("not json"), None);
    }
}
